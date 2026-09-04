import { useEffect, useRef, useState } from "react";
import {
  Check,
  ChevronDown,
  Cpu,
  Layers,
  Pencil,
  Pin,
  Plug,
  Plus,
  Trash2,
} from "lucide-react";
import type { UiSession } from "../types/ui";
import type {
  WorkspaceFileTreeEntry,
  WorkspaceInfo,
  WorkspaceOpenMode,
} from "../lib/workspaceBridge";
import { WorkspaceFilesPanel } from "./WorkspaceFilesPanel";

export type ResourceModalKind = "models" | "skills" | "plugins";

type SidebarProps = {
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

const normalizeRoot = (root?: string | null): string =>
  String(root || "")
    .trim()
    .replace(/^\\\\\?\\/, "")
    .replace(/\\/g, "/")
    .replace(/\/+$/, "")
    .toLowerCase();

const DEFAULT_DIRECTORY_ACTION = "workspace-action:defaultDirectory";
const CUSTOM_PATH_ACTION = "workspace-action:customPath";

export function Sidebar({
  sessions,
  currentSessionId,
  workspaces,
  activeWorkspaceRoot,
  runningSessionIds,
  completedSessionIds,
  workspaceCatalogError,
  onNewChat,
  onOpenWorkspace,
  onSelectWorkspace,
  onRetryWorkspaceCatalog,
  onResetWorkspaceCatalog,
  onSelectSession,
  onRenameSession,
  onDeleteSession,
  onOpenResource,
  onOpenFile,
}: SidebarProps) {
  const [renameSessionId, setRenameSessionId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [deleteSessionId, setDeleteSessionId] = useState<string | null>(null);
  const [pendingSessionId, setPendingSessionId] = useState<string | null>(null);
  const pendingSessionIdRef = useRef<string | null>(null);
  const [isWorkspacePickerOpen, setIsWorkspacePickerOpen] = useState(false);
  const workspacePickerRef = useRef<HTMLDivElement>(null);
  const [isExplorerOpen, setIsExplorerOpen] = useState(true);
  const [workspaceRecoveryAction, setWorkspaceRecoveryAction] = useState<"retry" | "reset" | null>(null);
  const currentSession =
    sessions.find((session) => session.id === currentSessionId) ?? null;
  const explorerSessionId =
    currentSession &&
    normalizeRoot(currentSession.cwd) === normalizeRoot(activeWorkspaceRoot)
      ? currentSession.id
      : undefined;
  const activeWorkspace = workspaces.find(
    (workspace) => normalizeRoot(workspace.root) === normalizeRoot(activeWorkspaceRoot),
  );

  useEffect(() => {
    if (!isWorkspacePickerOpen) return;
    const closeOnOutsidePointer = (event: PointerEvent) => {
      if (
        event.target instanceof Node
        && !workspacePickerRef.current?.contains(event.target)
      ) setIsWorkspacePickerOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setIsWorkspacePickerOpen(false);
    };
    document.addEventListener("pointerdown", closeOnOutsidePointer);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsidePointer);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [isWorkspacePickerOpen]);

  const chooseWorkspace = (root: string) => {
    setIsWorkspacePickerOpen(false);
    if (root === DEFAULT_DIRECTORY_ACTION) {
      onOpenWorkspace("defaultDirectory");
    } else if (root === CUSTOM_PATH_ACTION) {
      onOpenWorkspace("customPath");
    } else {
      onSelectWorkspace(root);
    }
  };
  const beginRename = (session: UiSession) => {
    setDeleteSessionId(null);
    setRenameSessionId(session.id);
    setRenameDraft(session.title);
  };

  const saveRename = async (sessionId: string) => {
    const title = renameDraft.trim();
    if (!title || pendingSessionId) return;
    pendingSessionIdRef.current = sessionId;
    setPendingSessionId(sessionId);
    try {
      await onRenameSession(sessionId, title);
      setRenameSessionId(null);
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

  return (
    <aside className="thinSidebar">
      <div className="thinSidebarWorkspaceControls">
        <button type="button" className="thinNewChatButton" onClick={onNewChat}>
          <Plus aria-hidden="true" />
          <span>New</span>
        </button>
      </div>

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
      ) : (
      <div
        className={`thinWorkspaceSelect ${isWorkspacePickerOpen ? "is-open" : ""}`}
        ref={workspacePickerRef}
      >
        <button
          type="button"
          className="thinWorkspaceSelectTrigger"
          aria-label="Current project"
          aria-haspopup="menu"
          aria-expanded={isWorkspacePickerOpen}
          onClick={() => setIsWorkspacePickerOpen((open) => !open)}
        >
          <span>{activeWorkspace?.name ?? "Select project..."}</span>
          <ChevronDown aria-hidden="true" />
        </button>
        {isWorkspacePickerOpen ? (
          <div className="thinWorkspaceSelectPanel" role="menu" aria-label="Projects">
            <button type="button" role="menuitem" onClick={() => chooseWorkspace(DEFAULT_DIRECTORY_ACTION)}>
              <span className="thinWorkspaceSelectCheck" />
              <span>Use default directory</span>
            </button>
            <button type="button" role="menuitem" onClick={() => chooseWorkspace(CUSTOM_PATH_ACTION)}>
              <span className="thinWorkspaceSelectCheck" />
              <span>Custom path...</span>
            </button>
            {workspaces.map((workspace) => {
              const selected = workspace.root === activeWorkspace?.root;
              return (
                <button
                  type="button"
                  role="menuitem"
                  className={selected ? "is-selected" : ""}
                  title={workspace.root}
                  key={workspace.root}
                  onClick={() => chooseWorkspace(workspace.root)}
                >
                  <span className="thinWorkspaceSelectCheck">
                    {selected ? <Check aria-hidden="true" /> : null}
                  </span>
                  <span>{workspace.name}</span>
                </button>
              );
            })}
          </div>
        ) : null}
      </div>
      )}

      <section className="thinSessionRegion" aria-label="会话">
        <div className="thinSectionHeader">
          <span>SESSIONS</span>
        </div>
        <div className="thinSessionList">
          {sessions.length === 0 ? (
            <div className="thinEmptyText">No sessions found</div>
          ) : (
            sessions.map((session) => {
              const isCurrent = session.id === currentSessionId;
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
                      onKeyDown={(event) => {
                        if (event.key === "Escape") setRenameSessionId(null);
                      }}
                      aria-label={`重命名 ${session.title}`}
                    />
                  </form>
                );
              }
              return (
                <div
                  className={`thinSessionRow ${isCurrent ? "is-active" : ""}`}
                  key={session.id}
                >
                  <button
                    type="button"
                    className="thinSessionSelect"
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
                    <button type="button" onClick={() => beginRename(session)} aria-label={`重命名 ${session.title}`}><Pencil aria-hidden="true" /></button>
                    <button
                      type="button"
                      title="删除会话"
                      onClick={() => { setRenameSessionId(null); setDeleteSessionId(session.id); }}
                      aria-label={`删除 ${session.title}`}
                    ><Trash2 aria-hidden="true" /></button>
                  </div>
                </div>
              );
            })
          )}
        </div>
      </section>

      <section className={`thinExplorerRegion ${isExplorerOpen ? "is-open" : ""}`}>
        <button
          type="button"
          className="thinExplorerHeader"
          onClick={() => setIsExplorerOpen((value) => !value)}
          aria-expanded={isExplorerOpen}
        >
          <ChevronDown aria-hidden="true" />
          <span>EXPLORER</span>
        </button>
        {isExplorerOpen ? (
          <WorkspaceFilesPanel
            isOpen
            sessionId={explorerSessionId}
            workspaceRoot={activeWorkspaceRoot ?? undefined}
            onOpenFile={onOpenFile}
          />
        ) : null}
      </section>

      <footer className="thinSidebarFooter">
        <button type="button" onClick={() => onOpenResource("models")}>
          <Cpu aria-hidden="true" /><span>Models</span>
        </button>
        <button type="button" onClick={() => onOpenResource("skills")}>
          <Layers aria-hidden="true" /><span>Skills</span>
        </button>
        <button type="button" onClick={() => onOpenResource("plugins")}>
          <Plug aria-hidden="true" /><span>Plugins</span>
        </button>
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
