import { recentWorkspaceGroups, sortSessionCatalog } from "./sidebarSessions";
import { SidebarWorkspaceGroup } from "./SidebarWorkspaceGroup";
import { t } from "../i18n";
import { useRef, useState } from "react";
import {
  Clock,
  GripVertical,
  Settings,


  Pencil,
  Pin,
  Blocks,
  SquarePen,
  Trash2,
} from "lucide-react";
import type { UiSession } from "../types/ui";
import type {
  WorkspaceFileTreeEntry,
  WorkspaceInfo,
  WorkspaceOpenMode,
} from "../lib/workspaceBridge";

export type ResourceModalKind = "models" | "skills" | "plugins";

type SidebarProps = {
  onExpandWorkspace?: (root: string) => void;
  onLoadMore?: (root: string) => void;
  hasMore?: (root: string) => boolean;
  onRenameWorkspace?: (root: string, name: string) => Promise<boolean>;
  onRemoveWorkspace?: (root: string) => Promise<boolean>;
  onReorderPinnedSessions?: (ids: string[]) => Promise<void>;
  onNavigate?: (page: "schedules" | "plugins" | "settings") => void;
  onPinSession?: (id: string, pinned: boolean) => Promise<void>;
  collapsed?: boolean;
  activePage?: string;
  sessions: UiSession[];
  currentSessionId: string | null;
  workspaces: WorkspaceInfo[];
  activeWorkspaceRoot: string | null;
  runningSessionIds: Set<string>;
  completedSessionIds: Set<string>;
  workspaceCatalogError: { message: string; canReset: boolean } | null;
  onNewChat: () => void;
  onOpenWorkspace: (mode: WorkspaceOpenMode) => void;
  onSelectWorkspace: (root: string) => void;
  onRetryWorkspaceCatalog: () => Promise<void>;
  onResetWorkspaceCatalog: () => Promise<void>;
  onSelectSession: (sessionId: string) => void;
  onRenameSession: (sessionId: string, title: string) => Promise<void>;
  onDeleteSession: (sessionId: string) => Promise<void>;
  onOpenResource: (kind: ResourceModalKind) => void;
  onOpenFile: (entry: WorkspaceFileTreeEntry) => void;
};

export function Sidebar({
  onExpandWorkspace, onLoadMore, hasMore, activeWorkspaceRoot,
  onRenameWorkspace,
  onRemoveWorkspace,
  onReorderPinnedSessions,
  workspaces,
  onNavigate,
  onPinSession,
  collapsed = false,
  activePage = "chat",
  sessions,
  currentSessionId,
  runningSessionIds,
  completedSessionIds,
  workspaceCatalogError,
  onNewChat,
  onRetryWorkspaceCatalog,
  onResetWorkspaceCatalog,
  onSelectSession,
  onRenameSession,
  onDeleteSession,
  onOpenResource,
}: SidebarProps) {
  const [renameSessionId, setRenameSessionId] = useState<string | null>(null);
  const renameSessionIdRef = useRef<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [deleteSessionId, setDeleteSessionId] = useState<string | null>(null);
  const [pendingSessionId, setPendingSessionId] = useState<string | null>(null);
  const pendingSessionIdRef = useRef<string | null>(null);
  const [workspaceRecoveryAction, setWorkspaceRecoveryAction] = useState<"retry" | "reset" | null>(null);
  const beginRename = (session: UiSession) => {
    setDeleteSessionId(null);
    renameSessionIdRef.current = session.id;
    setRenameSessionId(session.id);
    setRenameDraft(session.title);
  };

  const saveRename = async (sessionId: string) => {
    if (renameSessionIdRef.current !== sessionId || pendingSessionIdRef.current) return;
    const title = renameDraft.trim();
    if (!title || title === sessions.find((session) => session.id === sessionId)?.title) {
      renameSessionIdRef.current = null;
      setRenameSessionId(null);
      return;
    }
    pendingSessionIdRef.current = sessionId;
    setPendingSessionId(sessionId);
    try {
      await onRenameSession(sessionId, title);
      if (renameSessionIdRef.current === sessionId) {
        renameSessionIdRef.current = null;
        setRenameSessionId(null);
      }
    } catch {
      return;
    } finally {
      pendingSessionIdRef.current = null;
      setPendingSessionId(null);
    }
  };

  const confirmDelete = async (sessionId: string) => {
    if (pendingSessionId) return;
    pendingSessionIdRef.current = sessionId;
    setPendingSessionId(sessionId);
    try {
      await onDeleteSession(sessionId);
      setDeleteSessionId(null);
    } catch {
      return;
    } finally {
      pendingSessionIdRef.current = null;
      setPendingSessionId(null);
    }
  };

  const pinnedSessions = sortSessionCatalog(sessions.filter(session => session.isPinned));
  const draggedSession = useRef<string | null>(null);
  const [reordering, setReordering] = useState(false);
  const reorderPending = useRef(false);
  const savePinnedOrder = async (ids: string[]) => {
    if (!onReorderPinnedSessions || reorderPending.current) return;
    reorderPending.current = true;
    setReordering(true);
    try { await onReorderPinnedSessions(ids); } catch { /* Controller reports and reconciles failed writes. */ }
    finally { reorderPending.current = false; setReordering(false); }
  };
  const renderSession = (session: UiSession) => {

    const isCurrent = activePage === "chat" && session.id === currentSessionId;
    const isRunning = runningSessionIds.has(session.id);
    const isCompleted = completedSessionIds.has(session.id);
    const isRenaming = renameSessionId === session.id;
    const isDeleting = deleteSessionId === session.id;
    const isPending = pendingSessionId === session.id;
    if (isDeleting) {
      return (
        <div
          className="thinSessionRow thinSessionDeleteConfirm"
          key={session.id}
          role="alertdialog"
          aria-label={`Delete ${session.title || "New chat"}?`}
          onBlur={(event) => {
            if (
              !pendingSessionIdRef.current
              && !event.currentTarget.contains(event.relatedTarget)
            ) setDeleteSessionId(null);
          }}
          onKeyDown={(event) => {
            if (event.key === "Escape" && !isPending) setDeleteSessionId(null);
          }}
        >
          <span>{`Delete “${session.title || "New chat"}”`}</span>
          <button
            type="button"
            className="is-delete"
            disabled={isPending}
            onClick={() => void confirmDelete(session.id)}
          ><Trash2 aria-hidden="true" />Delete</button>
          <button
            type="button"
            autoFocus
            disabled={isPending}
            onClick={() => setDeleteSessionId(null)}
          >Cancel</button>
        </div>
      );
    }
    if (isRenaming) {
      return (
        <form
          className="thinSessionRow thinSessionInlineEdit"
          key={session.id}
          onSubmit={(event) => { event.preventDefault(); void saveRename(session.id); }}
        >
          <input
            autoFocus
            value={renameDraft}
            disabled={isPending}
            onChange={(event) => setRenameDraft(event.target.value)}
            onFocus={(event) => event.currentTarget.select()}
            onBlur={() => { void saveRename(session.id); }}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                renameSessionIdRef.current = null;
                setRenameSessionId(null);
              }
            }}
            aria-label={t("sidebar.renameValue", { value1: session.title })}
          />
        </form>
      );
    }
    return (
      <div
        className={`thinSessionRow ${isCurrent ? "is-active" : ""}`}
        key={session.id}
        onDragOver={event => { if (session.isPinned && draggedSession.current) event.preventDefault(); }}
        onDrop={event => {
          event.preventDefault();
          const source = draggedSession.current;
          draggedSession.current = null;
          if (!source || !session.isPinned || source === session.id) return;
          const bounds = event.currentTarget.getBoundingClientRect();
          const ids = pinnedSessions.map(item => item.id).filter(id => id !== source);
          ids.splice(ids.indexOf(session.id) + (event.clientY > bounds.top + bounds.height / 2 ? 1 : 0), 0, source);
          void savePinnedOrder(ids);
        }}
      >
        {session.isPinned && onReorderPinnedSessions && !hasMore?.("pinned") ? <button type="button" className="pinnedDragHandle" aria-label={`Reorder ${session.title}`} disabled={reordering} draggable={!reordering}
          onDragStart={event => { draggedSession.current = session.id; event.dataTransfer.effectAllowed = "move"; event.dataTransfer.setData("text/plain", session.id); }}
          onDragEnd={() => { draggedSession.current = null; }}
          onKeyDown={event => {
            if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
            event.preventDefault();
            const ids = pinnedSessions.map(item => item.id);
            const from = ids.indexOf(session.id), to = from + (event.key === "ArrowUp" ? -1 : 1);
            if (to < 0 || to >= ids.length) return;
            [ids[from], ids[to]] = [ids[to], ids[from]];
            void savePinnedOrder(ids);
          }}><GripVertical aria-hidden="true" /></button> : null}
        <button
          type="button"
          className="thinSessionSelect"
          title={session.cwd}
          aria-current={isCurrent ? "page" : undefined}
          onClick={() => onSelectSession(session.id)}
        >
          <span className={`thinSessionDot ${isRunning ? "is-running" : isCompleted || session.isUnread ? "is-unread" : ""}`} />
          <span className="thinSessionCopy">
            <strong>{session.title || "New chat"}</strong>
            <small>{`${formatRelativeTime(session.updatedAt)} · ${session.messageCount} ${session.messageCount === 1 ? "msg" : "msgs"}`}</small>
          </span>
          {session.isPinned ? <Pin className="thinPinnedIcon" aria-hidden="true" /> : null}
        </button>
        <div className="thinSessionActions">
          {onPinSession ? <button type="button" aria-label={`${session.isPinned ? "Unpin" : "Pin"} ${session.title}`} onClick={() => void onPinSession(session.id, !session.isPinned).catch(() => undefined)}><Pin aria-hidden="true" /></button> : null}
          <button type="button" onClick={() => beginRename(session)} aria-label={t("sidebar.renameValue", { value1: session.title })}><Pencil aria-hidden="true" /></button>
          <button
            type="button"
            title={t("sidebar.deleteConversation")}
            onClick={() => { setRenameSessionId(null); setDeleteSessionId(session.id); }}
            aria-label={t("sidebar.deleteValue", { value1: session.title })}
          ><Trash2 aria-hidden="true" /></button>
        </div>
      </div>
    );
  };

  return (
    <aside className={`thinSidebar ${collapsed ? "is-collapsed" : ""}`} aria-label="Main navigation">
      <nav className="appNavigation">
        <button type="button" aria-label="New chat" title="New chat" onClick={onNewChat}><SquarePen aria-hidden="true" /><span>New chat</span></button>
        <button type="button" aria-label="Scheduled" aria-current={activePage === "schedules" ? "page" : undefined} title="Scheduled" onClick={() => onNavigate?.("schedules")}><Clock aria-hidden="true" /><span>Scheduled</span></button>
        <button type="button" aria-label="Plugins" aria-current={activePage === "plugins" ? "page" : undefined} title="Plugins" onClick={() => onNavigate ? onNavigate("plugins") : onOpenResource("plugins")}><Blocks aria-hidden="true" /><span>Plugins</span></button>
      </nav>
      <div className="sidebarExpandedContent" hidden={collapsed}>
      {workspaceCatalogError ? (
        <div className="thinWorkspaceError" role="alert">
          <strong>{workspaceCatalogError.canReset ? "Workspace list is corrupted" : "Workspace list unavailable"}</strong>
          <span>{workspaceCatalogError.canReset ? "Retry, or reset only this list after confirmation." : "Fix the file access problem, then retry."}</span>
          <div>
            <button
              type="button"
              disabled={workspaceRecoveryAction !== null}
              onClick={() => {
                setWorkspaceRecoveryAction("retry");
                void onRetryWorkspaceCatalog().finally(() => setWorkspaceRecoveryAction(null));
              }}
            >Retry</button>
            {workspaceCatalogError.canReset ? (
              <button
                type="button"
                disabled={workspaceRecoveryAction !== null}
                onClick={() => {
                  setWorkspaceRecoveryAction("reset");
                  void onResetWorkspaceCatalog().finally(() => setWorkspaceRecoveryAction(null));
                }}
              >Reset list…</button>
            ) : null}
          </div>
        </div>
      ) : null}

      <section className="thinSessionRegion" aria-label="Pinned">
        <div className="thinSectionHeader"><span>Pinned</span></div>
        <div className="thinSessionList">{pinnedSessions.map(renderSession)}{hasMore?.("pinned") ? <button type="button" onClick={() => onLoadMore?.("pinned")}>Load more</button> : null}</div>
      </section>
      <section className="thinSessionRegion" aria-label="Recents">
        <div className="thinSectionHeader"><span>Recents</span></div>
        <div className="thinSessionList">{recentWorkspaceGroups(sessions, workspaces).map(group => (
          <SidebarWorkspaceGroup key={group.root} root={group.root} name={group.name} onRename={onRenameWorkspace} onRemove={onRemoveWorkspace} onExpand={onExpandWorkspace} initiallyExpanded={!onExpandWorkspace || group.root === activeWorkspaceRoot}>
            {group.sessions.map(renderSession)}
            {hasMore?.(group.root) ? <button type="button" onClick={() => onLoadMore?.(group.root)}>Load more</button> : null}
          </SidebarWorkspaceGroup>
        ))}</div>
      </section>
      </div>
      <footer className="thinSidebarFooter">
        <button type="button" aria-label="Settings" aria-current={activePage === "settings" ? "page" : undefined} title="Settings" onClick={() => onNavigate ? onNavigate("settings") : onOpenResource("models")}><Settings aria-hidden="true" /><span>Settings</span></button>
      </footer>
    </aside>
  );
}

const formatRelativeTime = (updatedAt?: number): string => {
  if (!updatedAt || !Number.isFinite(updatedAt)) return "just now";
  const elapsedMs = Math.max(0, Date.now() - updatedAt);
  const minutes = Math.floor(elapsedMs / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days}d ago`;
  return new Date(updatedAt).toLocaleDateString();
};

export default Sidebar;
