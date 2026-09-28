import { useEffect, useRef, useState } from "react";
import { FileDiff, FolderOpen, GitBranch, List, Bot, ListCollapse } from "lucide-react";
import { getWorkspaceGitStatus, type WorkspaceGitStatusResponse } from "../lib/workspaceBridge";
import { listProcesses } from "../lib/processBridge";
import type { UiSession } from "../types/ui";

export function WorkspaceOverview({
  name,
  root,
  agents,
  onFiles,
  onReview,
  onAgent,
  onTasks,
  sessionId,
}: {
  sessionId?: string | null;
  onTasks?: () => void;
  name: string;
  root: string | null;
  agents: UiSession[];
  onFiles: () => void;
  onReview: () => void;
  onAgent: (id: string, title: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [gitStatus, setGitStatus] = useState<WorkspaceGitStatusResponse | null>(null);
  const [gitError, setGitError] = useState("");
  useEffect(() => {
    if (!open || !root) return;
    let cancelled = false;
    setGitStatus(null);
    setGitError("");
    void getWorkspaceGitStatus(root)
      .then((status) => {
        if (!cancelled) setGitStatus(status);
      })
      .catch((error: unknown) => {
        if (!cancelled) setGitError(String(error));
      });
    return () => {
      cancelled = true;
    };
  }, [open, root]);
  const [taskCount, setTaskCount] = useState({ sessionId: "", count: 0 });
  useEffect(() => {
    if (!open || !sessionId || !onTasks) return;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    const refresh = async () => {
      try {
        const result = await listProcesses(sessionId);
        if (!disposed) setTaskCount({ sessionId, count: result.processes.length });
      } catch {
        if (!disposed) setTaskCount({ sessionId, count: 0 });
      } finally {
        if (!disposed) timer = setTimeout(refresh, 1500);
      }
    };
    void refresh();
    return () => { disposed = true; clearTimeout(timer); };
  }, [open, sessionId, onTasks]);
  const trigger = useRef<HTMLButtonElement>(null);
  const activate = (action: () => void) => {
    setOpen(false);
    action();
  };
  return (
    <div
      className="workspaceOverview"
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          setOpen(false);
          trigger.current?.focus();
        }
      }}

    >
      <button
        ref={trigger}
        type="button"
        className="nativePanelToggle"
        aria-label="Workspace overview"
        title="Workspace overview"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        <List aria-hidden="true" />
      </button>
      {open ? (
        <section className="workspaceOverviewPopover" aria-label="Workspace resources">
          <div className="workspaceOverviewName" title={root ?? undefined}>
            {name}
          </div>
          <button
            type="button"
            aria-label="Browse files"
            disabled={!root}
            onClick={() => activate(onFiles)}
          >
            <FolderOpen aria-hidden="true" />
            <span>Files</span>
          </button>
          {gitStatus?.isGitRepository ? (
            <>
              <div className="workspaceOverviewRow">
                <GitBranch aria-hidden="true" />
                <span>{gitStatus.branch || "Detached HEAD"}</span>
              </div>
              <button type="button" onClick={() => activate(onReview)}>
                <FileDiff aria-hidden="true" />
                <span>Review changes</span>
                <span className="workspaceOverviewStats">
                  <span>+{gitStatus.totalAdded}</span>
                  <span>−{gitStatus.totalRemoved}</span>
                </span>
              </button>
            </>
          ) : (
            <>
              {root ? (
                <button type="button" onClick={() => activate(onReview)}>
                  <FileDiff aria-hidden="true" />
                  <span>Review changes</span>
                </button>
              ) : null}
              <div className="workspaceOverviewHint">
                {gitError ||
                  (gitStatus
                    ? "Not a Git repository"
                    : root
                      ? "Loading Git status…"
                      : "No workspace selected")}
              </div>
            </>
          )}
          {onTasks && taskCount.sessionId === sessionId && taskCount.count > 0 ? <button type="button" aria-label="Task" onClick={()=>activate(onTasks)}>
            <ListCollapse aria-hidden="true"/><span>Task</span>
          </button> : null}
          {agents.length ? (
            <div className="workspaceOverviewAgents">
              {agents.map((agent) => (
                <button
                  type="button"
                  key={agent.id}
                  onClick={() => activate(() => onAgent(agent.id, agent.title))}
                >
                  <Bot aria-hidden="true" />
                  <span>{agent.title}</span>
                </button>
              ))}
            </div>
          ) : null}
        </section>
      ) : null}
    </div>
  );
}
