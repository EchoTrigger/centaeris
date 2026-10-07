import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { List } from "lucide-react";
import { useRouteLoaderData } from "react-router";
import { ApiError, apiJson } from "../api";
import { useTranslation } from "../i18n";
import type { AuthLoaderData, WorkspaceLoaderData } from "../router";
import { ShellPage } from "../shell/ShellPage";
import { AgentMessageList } from "../agent-chat/AgentMessageList";
import { AgentInputComposer } from "../agent-chat/AgentInputComposer";
import { AgentFilePreview } from "../agent-chat/AgentPreviewContent";
import { AgentOverviewPanel } from "../agent-chat/AgentOverviewPanel";
import { useAgentWorkSessions } from "../agent-chat/useAgentWorkSessions";
import { AgentPreviewPanel } from "../agent-chat/AgentPreviewPanel";
import { parsePreparedAgentInputPage } from "../agent-chat/preparedAgentInput";
import { AgentSessionPreviewShell } from "../agent-chat/AgentSessionPreviewShell";
import { AgentSessionActivityPreview } from "../agent-chat/AgentSessionActivityPreview";
import { AgentBreadcrumb } from "../agent-chat/SessionAgentBreadcrumb";
import { parseReferencedSession, type ReferencedSession } from "../agent-chat/agentOutputFeed";
import { agentHistoryPath, mergeAgentHistory, prependAgentHistory, parseAgentHistoryPage, projectAgentHistory, type AgentHistoryItem } from "../agent-chat/agentHistory";
import type { FilePreviewRequest } from "../agent-chat/agentChatViewTypes";
import "../agent-chat/agent-chat.css";

type Feed = Readonly<{
  items: readonly AgentHistoryItem[]; sessionId?: string;
  nextCursor: string | null; newestCursor: string | null; newerHasMore: boolean; hasMore: boolean; loading: boolean; error: boolean;
}>;
const emptyFeed: Feed = { items: [], nextCursor: null, newestCursor: null, newerHasMore: false, hasMore: false, loading: true, error: false };
type HistoryRequest = Readonly<{ cursor: string | null; replace: boolean; direction: "older" | "newer" }>;
const historyMessageHeight = (replies: HTMLElement) => replies.querySelector<HTMLElement>(".agentChatMessageList")?.getBoundingClientRect().height ?? 0;

export function AgentChatPageContent({ agentId }: Readonly<{ agentId: string }>) {
  const { t } = useTranslation();
  const { workspace, agents } = useRouteLoaderData("workspace") as WorkspaceLoaderData;
  const { user } = useRouteLoaderData("authenticated") as AuthLoaderData;
  const agent = agents.find(item => item.id === agentId);
  if (!agent || agent.workspaceId !== workspace.id || agent.status !== "active") throw new Error("agent_not_found");
  const [feed, setFeed] = useState<Feed>(emptyFeed);
  const feedRef = useRef(feed); feedRef.current = feed;
  const [sessions, setSessions] = useState<ReadonlyMap<string, ReferencedSession>>(new Map());
  const sessionsRef = useRef(sessions); sessionsRef.current = sessions;
  const [referenceError, setReferenceError] = useState(false);
  const [selection, setSelection] = useState<string | null>(null);
  const [panelOpen, setPanelOpen] = useState(false);
  const [filePreview, setFilePreview] = useState<FilePreviewRequest | null>(null);
  const filePreviewRef = useRef<FilePreviewRequest | null>(null);
  const [fileError, setFileError] = useState(false);
  const fileRequestRef = useRef<AbortController | null>(null);
  const [referenceTime, setReferenceTime] = useState(() => new Date().toISOString());
  const requestRef = useRef<AbortController | null>(null);
  const repliesRef = useRef<HTMLElement | null>(null);
  const planeRef = useRef<HTMLDivElement | null>(null);
  const composerDockRef = useRef<HTMLDivElement | null>(null);
  useLayoutEffect(() => {
    const plane = planeRef.current; const dock = composerDockRef.current;
    if (!plane || !dock) return;
    const measure = () => plane.style.setProperty("--agent-chat-composer-height", `${dock.getBoundingClientRect().height}px`);
    const observer = new ResizeObserver(measure); observer.observe(dock); measure();
    return () => observer.disconnect();
  }, []);
  const pageBusyRef = useRef(false);
  const failedRequestRef = useRef<HistoryRequest | null>(null);
  const scrollUpdateRef = useRef<{ items: readonly AgentHistoryItem[]; replace: boolean; older: boolean; top: number; messageHeight: number; following: boolean; followingTop: number } | null>(null);
  const finalFollowTopRef = useRef<number | null>(null);
  const allSessionsRef = useRef<ReadonlyMap<string, ReferencedSession>>(new Map());
  const workSessionsRef = useRef<ReadonlyMap<string, ReferencedSession>>(new Map());
  const clearWorksRef = useRef<() => void>(() => {});
  const clearAuthority = useCallback(() => {
    clearWorksRef.current();
    requestRef.current?.abort();
    failedRequestRef.current = null; scrollUpdateRef.current = null; finalFollowTopRef.current = null;
    fileRequestRef.current?.abort(); filePreviewRef.current = null; setFilePreview(null); setFileError(false);
    const cleared = { ...emptyFeed, loading: false, error: Boolean(feedRef.current.sessionId) };
    feedRef.current = cleared; setFeed(cleared);
    sessionsRef.current = new Map(); setSessions(sessionsRef.current);
    setSelection(null); setPanelOpen(false); setReferenceError(false);
  }, []);
  const works = useAgentWorkSessions(agentId, workspace.id, panelOpen && selection === null, clearAuthority);
  clearWorksRef.current = works.reset;
  workSessionsRef.current = new Map(works.sessions.map(session => [session.sessionId, session]));
  const allSessions = new Map(sessions);
  for (const session of works.sessions) allSessions.set(session.sessionId, { ...session, runState: sessions.get(session.sessionId)?.runState ?? session.runState });
  allSessionsRef.current = allSessions;
  const loadPage = useCallback(async (afterCursor: string | null, replace = false, direction: "older" | "newer" = "newer") => {
    if (pageBusyRef.current) return;
    pageBusyRef.current = true;
    const older = replace || direction === "older";
    failedRequestRef.current = null; finalFollowTopRef.current = null;
    requestRef.current?.abort();
    const controller = new AbortController(); requestRef.current = controller;
    const signal = controller.signal;
    const scrollStart = repliesRef.current;
    const followingAtStart = scrollStart ? { top: scrollStart.scrollTop, following: scrollStart.scrollHeight - scrollStart.clientHeight - scrollStart.scrollTop <= 24 } : null;
    setFeed(previous => ({ ...previous, loading: true, error: false }));
    try {
      const value = await apiJson<unknown>(agentHistoryPath(agentId, afterCursor, 50, older ? "older" : "newer"), { signal });
      const page = parseAgentHistoryPage(value, agentId, afterCursor, replace ? undefined : feedRef.current.sessionId, 50, older ? "older" : "newer");
      if (signal.aborted) return;
      const previous = feedRef.current;
      const items = replace ? page.items : older ? prependAgentHistory(previous.items, page.items) : mergeAgentHistory(previous.items, page.items);
      const replies = repliesRef.current;
      // Capture at response time so scrolling during the request remains the
      // user's current reading position and choice to follow the latest reply.
      if (replies) {
        // Retain following only while they have not moved up to read history.
        const stillFollowing = Boolean(followingAtStart?.following && replies.scrollTop >= followingAtStart.top - 24);
        scrollUpdateRef.current = { items, replace, older, top: replies.scrollTop, messageHeight: historyMessageHeight(replies), following: stillFollowing || replies.scrollHeight - replies.clientHeight - replies.scrollTop <= 24, followingTop: stillFollowing ? followingAtStart!.top : replies.scrollTop };
      }
      const nextFeed = { items, sessionId: page.sessionId,
        nextCursor: older ? page.nextCursor : previous.nextCursor,
        hasMore: older ? page.hasMore : previous.hasMore,
        newestCursor: replace || !older ? page.newestCursor : previous.newestCursor,
        newerHasMore: !older && page.hasMore, loading: true, error: false };
      feedRef.current = nextFeed; setFeed(nextFeed); setReferenceTime(new Date().toISOString());
      // The history cursor advances only for new rows. Uptake updates an old
      // input, so reread the latest loaded input separately even on an empty tail.
      const latestInput = [...items].reverse().find(item => item.kind === "input");
      if (latestInput?.kind === "input") {
        try {
          const afterSequence = latestInput.input.sequence - 1;
          const query = new URLSearchParams({ afterSequence: String(afterSequence), limit: "1" });
          const value = await apiJson<unknown>(`/api/agents/${encodeURIComponent(agentId)}/inputs?${query}`, { signal });
          const inputPage = parsePreparedAgentInputPage(value, { agentId, sessionId: page.sessionId }, afterSequence, 1);
          if (!inputPage.inputs[0] || inputPage.inputs[0].inputId !== latestInput.input.inputId) throw new Error("agent_input_refresh_mismatch");
          if (signal.aborted) return;
          const refreshed = mergeAgentHistory(feedRef.current.items, [{ ...latestInput, input: inputPage.inputs[0] }]);
          if (scrollUpdateRef.current) scrollUpdateRef.current.items = refreshed;
          const refreshedFeed = { ...feedRef.current, items: refreshed }; feedRef.current = refreshedFeed; setFeed(refreshedFeed);
        } catch (error) {
          if (signal.aborted) return;
          if (error instanceof ApiError && [401, 403, 404].includes(error.status)) throw error;
          // Keep the last read receipt; the existing next refresh retries it.
        }
      }
      const referencedIds = new Set(items.flatMap(item => item.kind === "message" ? [...item.message.sessionRefs] : []));
      if (replace) {
        const retained = new Map([...sessionsRef.current].filter(([id]) => referencedIds.has(id)));
        sessionsRef.current = retained; setSessions(retained);
        setSelection(previous => previous && (referencedIds.has(previous) || workSessionsRef.current.has(previous)) ? previous : null);
      }
      // Hydrate only server-supplied references, once each, with four bounded GET
      // workers. A failed or foreign reference never becomes a guessed title.
      // Retry unresolved references from all loaded rows even when this page is
      // empty. Reference failure does not roll back the committed history cursor.
      const ids = [...referencedIds].filter(id => !allSessionsRef.current.has(id));
      setReferenceError(false);
      let index = 0;
      await Promise.all(Array.from({ length: Math.min(4, ids.length) }, async () => {
        while (index < ids.length && !signal.aborted) {
          const id = ids[index++];
          try {
            const value = await apiJson<unknown>(`/api/sessions/${encodeURIComponent(id)}`, { signal });
            const session = parseReferencedSession(value, workspace.id, id);
            if (!signal.aborted) {
              const replies = repliesRef.current;
              // The older rows may already be visible while their references
              // load. Preserve the user's current position for this next commit.
              if (older && !replace && replies && !scrollUpdateRef.current) scrollUpdateRef.current = { items: feedRef.current.items, replace: false, older: true, top: replies.scrollTop, messageHeight: historyMessageHeight(replies), following: false, followingTop: replies.scrollTop };
              setSessions(previous => {
                const next = new Map(previous); next.set(id, session); sessionsRef.current = next; return next;
              });
            }
          } catch { if (!signal.aborted) setReferenceError(true); }
        }
      }));
      if (!signal.aborted) setFeed(previous => ({ ...previous, loading: false }));
    } catch (error) {
      if (signal.aborted) return;
      if (error instanceof ApiError && [401, 403, 404].includes(error.status)) clearAuthority();
      else {
        failedRequestRef.current = { cursor: afterCursor, replace, direction };
        setFeed(previous => ({ ...previous, loading: false, error: true }));
      }
    } finally { if (requestRef.current === controller) pageBusyRef.current = false; }
  }, [agentId, workspace.id, clearAuthority]);
  useEffect(() => {
    pageBusyRef.current = false; failedRequestRef.current = null; scrollUpdateRef.current = null; finalFollowTopRef.current = null;
    feedRef.current = emptyFeed; sessionsRef.current = new Map();
    fileRequestRef.current?.abort(); filePreviewRef.current = null; setFilePreview(null); setFileError(false);
    setFeed(emptyFeed); setSessions(new Map()); setSelection(null); setPanelOpen(false);
    void loadPage(null, true);
    return () => { requestRef.current?.abort(); fileRequestRef.current?.abort(); };
  }, [loadPage]);
  useEffect(() => {
    if (!feed.sessionId || feed.loading || feed.error) return;
    const timer = setTimeout(() => { void loadPage(feedRef.current.newestCursor, feedRef.current.newestCursor === null); }, feed.newerHasMore ? 0 : 5000);
    return () => { clearTimeout(timer); };
  }, [feed.sessionId, feed.loading, feed.error, feed.newerHasMore, loadPage]);
  // biome-ignore lint/correctness/useExhaustiveDependencies: Session metadata changes the measured message-list height after the older rows commit.
  useLayoutEffect(() => {
    const replies = repliesRef.current;
    const update = scrollUpdateRef.current;
    if (!replies) return;
    if (update && update.items === feed.items) {
      scrollUpdateRef.current = null;
      if (update.replace || (!update.older && update.following)) {
        replies.scrollTop = replies.scrollHeight;
        if (feed.loading) finalFollowTopRef.current = replies.scrollTop;
      } else if (update.older) replies.scrollTop = update.top + historyMessageHeight(replies) - update.messageHeight;
    }
    // References settle after the message commit. Finish
    // following only while the user has kept that position instead of reading up.
    if (!feed.loading) {
      const followedTop = finalFollowTopRef.current; finalFollowTopRef.current = null;
      if (!feed.error && followedTop !== null && replies.scrollTop >= followedTop) replies.scrollTop = replies.scrollHeight;
    }
  }, [feed.items, feed.loading, feed.error, sessions]);
  const retryHistory = () => {
    const failed = failedRequestRef.current;
    void (failed ? loadPage(failed.cursor, failed.replace, failed.direction) : loadPage(null, true));
  };
  const loadOlder = useCallback(() => {
    const current = feedRef.current;
    if (current.hasMore && !current.loading && !current.error && current.nextCursor) void loadPage(current.nextCursor, false, "older");
  }, [loadPage]);
  const onRepliesScroll = () => {
    const replies = repliesRef.current; if (!replies) return;
    if (finalFollowTopRef.current !== null && replies.scrollTop < finalFollowTopRef.current) finalFollowTopRef.current = null;
    const update = scrollUpdateRef.current;
    if (update && !update.replace) {
      const nearBottom = replies.scrollHeight - replies.clientHeight - replies.scrollTop <= 24;
      if (nearBottom && !update.following) update.followingTop = replies.scrollTop;
      update.following = nearBottom || (update.following && replies.scrollTop >= update.followingTop - 24);
      update.top = replies.scrollTop; update.messageHeight = historyMessageHeight(replies);
    }
    if (replies.scrollTop <= 96) loadOlder();
  };
  useEffect(() => {
    const replies = repliesRef.current;
    if (!feed.loading && !feed.error && feed.hasMore && feed.nextCursor && replies && replies.scrollTop <= 96) loadOlder();
  }, [feed.loading, feed.error, feed.hasMore, feed.nextCursor, loadOlder]);
  const refreshAccepted = useCallback(async () => {
    await loadPage(feedRef.current.newestCursor, !feedRef.current.sessionId || feedRef.current.newestCursor === null);
  }, [loadPage]);

  const updateRunState = useCallback((id: string, runState: "running" | "unknown") => {
    setSessions(previous => {
      const session = previous.get(id) ?? allSessionsRef.current.get(id);
      if (!session || session.runState === runState) return previous;
      const next = new Map(previous); next.set(id, { ...session, runState }); sessionsRef.current = next; return next;
    });
  }, []);
  const selected = selection ? allSessions.get(selection) : undefined;
  const returnToOverview = () => { fileRequestRef.current?.abort(); filePreviewRef.current = null; setSelection(null); setFilePreview(null); setFileError(false); setPanelOpen(true); };
  const selectSession = (id: string) => {
    if (!allSessions.has(id)) throw new Error("agent_session_reference_invalid");
    fileRequestRef.current?.abort(); filePreviewRef.current = null; setFilePreview(null); setFileError(false); setSelection(id); setPanelOpen(true);
  };
  const previewFile = useCallback(async (request: FilePreviewRequest, sessionId: string) => {
    fileRequestRef.current?.abort(); const controller = new AbortController(); fileRequestRef.current = controller;
    filePreviewRef.current = null; setFileError(false); setFilePreview(null); setPanelOpen(true);
    try {
      let session = allSessionsRef.current.get(sessionId);
      if (!session) {
        session = parseReferencedSession(await apiJson<unknown>(`/api/sessions/${encodeURIComponent(sessionId)}`, { signal: controller.signal }), workspace.id, sessionId);
      }
      if (controller.signal.aborted) return;
      const next = new Map(sessionsRef.current); next.set(sessionId, session); sessionsRef.current = next; setSessions(next);
      setSelection(sessionId); filePreviewRef.current = request; setFilePreview(request);
    } catch { if (!controller.signal.aborted) setFileError(true); }
  }, [workspace.id]);
  const unavailablePreview = useCallback(() => {
    if (!filePreview || filePreviewRef.current !== filePreview) return;
    fileRequestRef.current?.abort(); filePreviewRef.current = null; setFilePreview(null); setFileError(true);
    filePreview.invalidateMetadata();
  }, [filePreview]);
  return <ShellPage initialTab="chat" activeAgent={agent} mainClassName="agentChatMain" showRecentSessions recentRevision={referenceTime}>
    <header className="agentChatRouteHeader">
      <AgentBreadcrumb agent={agent} label={t("agentChat.path")} />
      <button className={`agentChatPanelToggle ${panelOpen ? "isActive" : ""}`} type="button" aria-label={t("agentChat.toggleOverview")} title={t("agentChat.toggleOverview")} aria-expanded={panelOpen} aria-controls="agent-chat-preview" onClick={() => setPanelOpen(value => !value)}><List aria-hidden="true" /></button>
    </header>
    <div className="agentChatRoutePlane" ref={planeRef}>
      <div className="agentChatConversationColumn">
      <section ref={repliesRef} onScroll={onRepliesScroll} className="agentChatRouteReplies" aria-label={t("agentChat.replies")}>
        <div className="agentChatContentWidth">
        {feed.error ? <div role="alert"><p>{t("agentChat.loadError")}</p><button type="button" onClick={retryHistory}>{t("agentChat.retry")}</button></div> : null}
        {referenceError ? <p role="alert">{t("agentChat.referencesError")}</p> : null}
        {fileError ? <p role="alert">{t("agentChat.filesError")}</p> : null}
        <AgentMessageList messages={projectAgentHistory(feed.items, t("agentChat.you"), agent.name, allSessions, !feed.error && !feed.newerHasMore, feed.sessionId ? { agentId, sessionId: feed.sessionId } : undefined)} referenceTime={referenceTime} readLabel={t("agentChat.read")} onPreviewFile={(file, sessionId) => void previewFile(file, sessionId)} onPreviewSession={selectSession} />
        </div>
      </section>
      <div className="agentChatComposerDock" ref={composerDockRef}><AgentInputComposer key={JSON.stringify([user.id, workspace.id, agentId])} agentId={agentId} workspaceId={workspace.id} userId={user.id} sessionId={feed.sessionId} onAccepted={refreshAccepted} onAuthorityLost={clearAuthority} /></div>
      </div>
      {panelOpen ? <div className="agentChatPreviewDock" id="agent-chat-preview"><AgentPreviewPanel key={agentId} label={t(selected ? "agentChat.preview" : "agentChat.overview")} focusKey={JSON.stringify([agentId, selection, filePreview?.file.objectRef ?? null])} onClose={() => setPanelOpen(false)}>{selected ? <AgentSessionPreviewShell
        embedded
        session={selected} agentLabel={agent.name} labels={{ preview: t("agentChat.preview"), path: t("agentChat.path"), close: t("agentChat.close"), loading: t("agentChat.loading"), retry: t("agentChat.retry"), returnToSession: t("agentChat.returnToSession") }}
        load={{ status: "ready" }}
        conversation={<AgentSessionActivityPreview key={selected.sessionId} sessionId={selected.sessionId} onRunState={updateRunState} />}
        libraryPreview={filePreview ? { title: filePreview.file.displayName, content: <AgentFilePreview key={filePreview.file.previewUrl} file={filePreview.file} onUnavailable={unavailablePreview} /> } : undefined}
        onReturnToSession={() => { filePreviewRef.current = null; setFilePreview(null); }} onReturn={returnToOverview} onClose={() => setPanelOpen(false)}
      /> : <AgentOverviewPanel key={agentId} agent={agent} revision={`${referenceTime}:${works.revision}`} sessions={works.sessions.filter(session => session.sessionId !== feed.sessionId)} pagination={works} onPreviewSession={selectSession} onPreviewFile={(file, sessionId) => void previewFile(file, sessionId)} onRunState={updateRunState} />}</AgentPreviewPanel></div> : null}
    </div>
  </ShellPage>;
}
