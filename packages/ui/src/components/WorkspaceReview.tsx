import { lazy, Suspense, useEffect, useRef, useState } from "react";
import {
  ChevronDown,
  ChevronRight,
  MoreHorizontal,
  RefreshCw,
  ArrowUp,
  ArrowDown,
} from "lucide-react";
import {
  getWorkspaceGitView,
  stageWorkspaceGitFile,
  unstageWorkspaceGitFile,
  type WorkspaceGitSource,
  type WorkspaceGitView,
  type WorkspaceGitFileDiffResponse,
} from "../lib/workspaceBridge";
const CodePreview = lazy(() => import("./CodePreview"));

export function WorkspaceReview({
  root,
  onOpenFile,
}: {
  root: string;
  onOpenFile?: (path: string) => void;
}) {
  return <Review key={root} root={root} onOpenFile={onOpenFile} />;
}
function Review({ root, onOpenFile }: { root: string; onOpenFile?: (path: string) => void }) {
  const [source, setSource] = useState<WorkspaceGitSource>("unstaged");
  const [baseInput, setBaseInput] = useState("");
  const [base, setBase] = useState<string>();
  const [revision, setRevision] = useState(0);
  const [view, setView] = useState<WorkspaceGitView | null>(null);
  const [loading, setLoading] = useState(true);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [query, setQuery] = useState("");
  const [match, setMatch] = useState(0);
  const [diff, setDiff] = useState<WorkspaceGitFileDiffResponse | null>(null);
  const [diffLoading, setDiffLoading] = useState(false);
  const [error, setError] = useState("");
  const [diffError, setDiffError] = useState("");
  const [actionError, setActionError] = useState("");
  const [busy, setBusy] = useState(false);
  const writing = useRef(false);
  const alive = useRef(true);
  const container = useRef<HTMLDivElement>(null);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const baseRef = source === "branch" ? base : undefined;
  // biome-ignore lint/correctness/useExhaustiveDependencies: Manual/focus refresh invalidates this live read.
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError("");
    setView(null);
    void getWorkspaceGitView(root, source, baseRef)
      .then((value) => {
        if (!cancelled) setView(value);
      })
      .catch((error: unknown) => {
        if (!cancelled) setError(String(error));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [root, source, baseRef, revision]);
  useEffect(() => {
    if (typeof window === "undefined") return;
    const refresh = () => {
      if (
        !writing.current &&
        document.visibilityState === "visible" &&
        container.current?.getClientRects().length
      )
        setRevision((value) => value + 1);
    };
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    const timer = window.setInterval(refresh, 60_000);
    return () => {
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
      window.clearInterval(timer);
    };
  }, []);
  const files =
    view?.files.filter((file) =>
      `${file.path} ${file.originalPath ?? ""}`.toLowerCase().includes(filter.toLowerCase()),
    ) ?? [];
  const selected = files.find((file) => file.path === expanded);
  const selectedPath = selected?.path;
  const selectedStatus = selected?.status;
  useEffect(() => {
    let cancelled = false;
    setDiff(null);
    setDiffError("");
    setMatch(0);
    setDiffLoading(false);
    if (view && selectedPath && selectedStatus !== "?" && selectedStatus !== "U") {
      setDiffLoading(true);
      void getWorkspaceGitView(root, source, baseRef, selectedPath)
        .then((value) => {
          if (!cancelled) setDiff(value.diff);
        })
        .catch((error: unknown) => {
          if (!cancelled) setDiffError(String(error));
        })
        .finally(() => {
          if (!cancelled) setDiffLoading(false);
        });
    }
    return () => {
      cancelled = true;
    };
  }, [root, source, baseRef, view, selectedPath, selectedStatus]);
  const matches =
    query && diff
      ? diff.diffPreview
          .split("\n")
          .flatMap((line, index) =>
            line.toLowerCase().includes(query.toLowerCase()) ? [index + 1] : [],
          )
      : [];
  const matchIndex = matches.length ? match % matches.length : 0;
  const blocked = busy || loading || diffLoading || !view || view.hasConflicts;
  async function changeIndex() {
    if (writing.current || blocked || !view || !selected || source === "branch") return;
    writing.current = true;
    setBusy(true);
    setActionError("");
    try {
      await (source === "staged" ? unstageWorkspaceGitFile : stageWorkspaceGitFile)(
        root,
        selected.path,
        view.snapshot,
      );
    } catch (error) {
      if (alive.current) setActionError(String(error));
    } finally {
      writing.current = false;
      if (alive.current) {
        setView(null);
        setBusy(false);
        setRevision((value) => value + 1);
      }
    }
  }
  async function copyPath(absolute: boolean) {
    if (!selected) return;
    const path = absolute ? `${root.replace(/[\\/]$/, "")}/${selected.path}` : selected.path;
    try {
      await navigator.clipboard.writeText(path);
    } catch (error) {
      if (alive.current) setActionError(String(error));
    }
  }
  return (
    <div className="workspaceReview" ref={container}>
      <div className="workspaceReviewToolbar">
        <select
          aria-label="Change source"
          value={source}
          disabled={busy}
          onChange={(event) => {
            setSource(event.target.value as WorkspaceGitSource);
            setExpanded(null);
            setActionError("");
          }}
        >
          <option value="unstaged">Unstaged</option>
          <option value="staged">Staged</option>
          <option value="branch">Branch</option>
        </select>
        <span className="workspaceReviewBranch">{view?.branch ?? ""}</span>
        <button
          type="button"
          aria-label="Refresh"
          title="Refresh"
          disabled={busy || loading}
          onClick={() => setRevision((value) => value + 1)}
        >
          <RefreshCw size={14} />
        </button>
      </div>
      {source === "branch" ? (
        <div className="workspaceReviewToolbar">
          <input
            aria-label="Base reference"
            placeholder={view?.baseRef ?? "Base branch or ref"}
            value={baseInput}
            onChange={(event) => setBaseInput(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                setBase(baseInput.trim() || undefined);
                setRevision((value) => value + 1);
              }
            }}
          />
          <button
            type="button"
            disabled={busy || loading}
            onClick={() => {
              setBase(baseInput.trim() || undefined);
              setRevision((value) => value + 1);
            }}
          >
            Compare
          </button>
        </div>
      ) : null}
      <div className="workspaceReviewToolbar">
        <input
          aria-label="Filter files"
          placeholder="Filter files…"
          value={filter}
          onChange={(event) => setFilter(event.target.value)}
        />
        <span>{files.length}</span>
      </div>
      {error ? <p role="alert">{error}</p> : null}
      {actionError ? <p role="alert">{actionError}</p> : null}
      {loading ? (
        <p>Loading changes…</p>
      ) : view && !files.length ? (
        <p>{filter ? "No matching files" : "No changes"}</p>
      ) : null}
      <div className="workspaceReviewChanges">
        {files.map((file) => {
          const open = selectedPath === file.path;
          return (
            <section className="workspaceReviewChange" key={file.path}>
              <button
                type="button"
                className="workspaceReviewHeading"
                aria-label={`${open ? "Collapse" : "Expand"} ${file.path}`}
                aria-expanded={open}
                onClick={() => setExpanded(open ? null : file.path)}
              >
                {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                <span className="workspaceReviewStatus">{file.status}</span>
                <span
                  className="workspaceReviewPath"
                  title={file.originalPath ? `${file.originalPath} → ${file.path}` : file.path}
                >
                  {file.originalPath ? `${file.originalPath} → ` : ""}
                  {file.path}
                </span>
                <span className="workspaceReviewStats">
                  {file.added !== null && file.removed !== null ? (
                    <>
                      <span>+{file.added}</span>
                      <span>−{file.removed}</span>
                    </>
                  ) : file.status === "?" ? (
                    "New"
                  ) : file.status === "U" ? (
                    "Conflict"
                  ) : (
                    "—"
                  )}
                </span>
              </button>
              {open ? (
                <>
                  <div className="workspaceReviewToolbar workspaceReviewFileTools">
                    <button type="button" onClick={() => onOpenFile?.(file.path)}>
                      Open file
                    </button>
                    <details className="workspaceReviewMenu">
                      <summary aria-label="File actions">
                        <MoreHorizontal size={16} />
                      </summary>
                      <div>
                        <button type="button" onClick={() => void copyPath(false)}>
                          Copy relative path
                        </button>
                        <button type="button" onClick={() => void copyPath(true)}>
                          Copy absolute path
                        </button>
                        {source !== "branch" && file.status !== "U" ? (
                          <button
                            type="button"
                            disabled={blocked}
                            aria-label={`${source === "staged" ? "Unstage" : "Stage"} ${file.path}`}
                            onClick={() => void changeIndex()}
                          >
                            {source === "staged" ? "Unstage" : "Stage"}
                          </button>
                        ) : null}
                      </div>
                    </details>
                  </div>
                  {file.status === "?" ? (
                    <p>Untracked file</p>
                  ) : file.status === "U" ? (
                    <p>Unmerged file</p>
                  ) : diffError ? (
                    <p role="alert">{diffError}</p>
                  ) : diffLoading ? (
                    <p>Loading diff…</p>
                  ) : diff ? (
                    <>
                      <div className="workspaceReviewToolbar workspaceReviewFind">
                        <input
                          aria-label="Find in diff"
                          placeholder="Find in diff…"
                          value={query}
                          onChange={(event) => {
                            setQuery(event.target.value);
                            setMatch(0);
                          }}
                        />
                        {query ? (
                          <span>
                            {matches.length ? matchIndex + 1 : 0}/{matches.length}
                          </span>
                        ) : null}
                        <button
                          type="button"
                          aria-label="Previous match"
                          disabled={!matches.length}
                          onClick={() =>
                            setMatch((matchIndex + matches.length - 1) % matches.length)
                          }
                        >
                          <ArrowUp size={14} />
                        </button>
                        <button
                          type="button"
                          aria-label="Next match"
                          disabled={!matches.length}
                          onClick={() => setMatch((matchIndex + 1) % matches.length)}
                        >
                          <ArrowDown size={14} />
                        </button>
                      </div>
                      {diff.truncated ? (
                        <p>Diff truncated. Only the available preview is shown.</p>
                      ) : null}
                      <Suspense fallback={<p>Loading preview…</p>}>
                        <CodePreview
                          content={diff.diffPreview || "No textual diff available."}
                          path={file.path}
                          variant="diff"
                          targetLine={matches[matchIndex]}
                        />
                      </Suspense>
                    </>
                  ) : (
                    <p>No textual diff available.</p>
                  )}
                </>
              ) : null}
            </section>
          );
        })}
      </div>
    </div>
  );
}
