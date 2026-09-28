import { t } from "../i18n";
import { type RefObject, useEffect, useMemo, useRef, useState } from "react";
import { RefreshCw, ChevronRight, FileText, Image } from "lucide-react";
import {
  getWorkspaceFileTree,
  type WorkspaceFileTreeEntry,
  type WorkspaceFileTreeResponse,
} from "../lib/workspaceBridge";

type WorkspaceFilesPanelProps = {
  isOpen: boolean;
  sessionId?: string;
  workspaceRoot?: string;
  focusedPath?: string;
  focusedLine?: number;
  onOpenFile?: (entry: WorkspaceFileTreeEntry) => void;
};

type WorkspaceFilesState = {
  tree: WorkspaceFileTreeResponse | null;
  error: string;
  loading: boolean;
};

const normalizeEntryPath = (path?: string): string =>
  String(path || "")
    .replace(/\\/g, "/")
    .replace(/^\/+/, "")
    .trim()
    .toLowerCase();

const isFocusedEntry = (
  entry: WorkspaceFileTreeEntry,
  focusedPath: string,
): boolean =>
  !entry.isDirectory && normalizeEntryPath(entry.path) === focusedPath;

const isFocusedAncestor = (
  entry: WorkspaceFileTreeEntry,
  focusedPath: string,
): boolean => {
  if (!entry.isDirectory || !focusedPath) {
    return false;
  }
  const entryPath = normalizeEntryPath(entry.path);
  return Boolean(
    entryPath &&
    (focusedPath === entryPath || focusedPath.startsWith(`${entryPath}/`)),
  );
};

const isPreviewImagePath = (path: string): boolean =>
  /\.(png|jpe?g|gif|webp|bmp|ico|svg)$/i.test(path.trim());

const renderFileEntry = (
  entry: WorkspaceFileTreeEntry,
  onOpenFile: ((entry: WorkspaceFileTreeEntry) => void) | undefined,
  focusedPath: string,
  selectedRef: RefObject<HTMLButtonElement | null>,
  depth = 0,
) => {
  const EntryIcon = entry.isDirectory
    ? ChevronRight
    : isPreviewImagePath(entry.path)
      ? Image
      : FileText;
  const focused = isFocusedEntry(entry, focusedPath);
  const containsFocus = isFocusedAncestor(entry, focusedPath);
  const content = (
    <div
      className="workspaceFilesEntryLabel"
      style={{ paddingLeft: `${depth * 14}px` }}
    >
      <EntryIcon className="workspaceFilesEntryIcon" aria-hidden="true" />
      <span className="workspaceFilesEntryName">{entry.name}</span>
    </div>
  );

  if (!entry.isDirectory) {
    return (
      <button
        type="button"
        className={`workspaceFilesEntry workspaceFilesFileButton ${focused ? "is-focused" : ""}`}
        key={entry.path}
        ref={focused ? selectedRef : undefined}
        aria-label={t("toolActivityTranscript.openValue", { value1: entry.path })}
        onClick={() => onOpenFile?.(entry)}
      >
        {content}
      </button>
    );
  }

  return (
    <details
      className="workspaceFilesEntry is-directory"
      key={entry.path}
      aria-label={entry.path}
      open={containsFocus || undefined}
    >
      <summary>{content}</summary>
      {entry.children.length > 0 ? (
        <div className="workspaceFilesChildren">
          {entry.children.map((child) =>
            renderFileEntry(
              child,
              onOpenFile,
              focusedPath,
              selectedRef,
              depth + 1,
            ),
          )}
        </div>
      ) : null}
    </details>
  );
};

export function WorkspaceFilesPanel({
  isOpen,
  sessionId,
  workspaceRoot,
  focusedPath,
  focusedLine,
  onOpenFile,
}: WorkspaceFilesPanelProps) {
  const [state, setState] = useState<WorkspaceFilesState>({
    tree: null,
    error: "",
    loading: false,
  });
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    if (!isOpen || typeof window === "undefined") return;
    const refresh = () => { if (document.visibilityState === "visible") setRevision(v => v + 1); };
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => { window.removeEventListener("focus", refresh); document.removeEventListener("visibilitychange", refresh); };
  }, [isOpen]);
  const selectedFileRef = useRef<HTMLButtonElement | null>(null);
  const normalizedFocusedPath = useMemo(
    () => normalizeEntryPath(focusedPath),
    [focusedPath],
  );

  useEffect(() => {
    if (!isOpen) {
      return;
    }
    if (!workspaceRoot) {
      setState({ tree: null, loading: false, error: "" });
      return;
    }
    let cancelled = false;
    setState(previous => ({ ...previous, loading: true, error: "" }));
    getWorkspaceFileTree(12, sessionId, workspaceRoot)
      .then((tree) => {
        if (cancelled) {
          return;
        }
        setState({ tree, error: "", loading: false });
      })
      .catch((error) => {
        if (cancelled) {
          return;
        }
        setState({
          tree: null,
          error:
            error instanceof Error ? error.message : t("workspaceFilesPanel.unableToReadWorkspaceFiles"),
          loading: false,
        });
      });
    return () => {
      cancelled = true;
    };
  }, [sessionId, isOpen, workspaceRoot, revision]);

  useEffect(() => {
    if (!isOpen || !normalizedFocusedPath || !state.tree) {
      return;
    }
    const timer = window.setTimeout(() => {
      selectedFileRef.current?.scrollIntoView({
        block: "center",
        inline: "nearest",
        behavior: "smooth",
      });
    }, 80);
    return () => window.clearTimeout(timer);
  }, [isOpen, normalizedFocusedPath, state.tree]);

  return (
    <section className="workspaceFilesPanel">
      <header className="workspaceFilesHeader">
        <span className="workspaceFilesScope">{t("workspaceFilesPanel.allFiles")}</span>
        <button type="button" aria-label="Refresh files" title="Refresh files" disabled={state.loading} onClick={() => setRevision(v => v + 1)}>
          <RefreshCw className="workspaceFilesScopeIcon" aria-hidden="true" />
        </button>
        {focusedLine ? (
          <span className="workspaceFilesFocusLine">{t("workspaceFilesPanel.lines")}{" "}{focusedLine}</span>
        ) : null}
      </header>
      <div className="workspaceFilesBody">
        {state.loading ? (
          <div className="workspaceFilesHint">{t("workspaceFilesPanel.loadingWorkspace")}</div>
        ) : null}
        {state.error ? (
          <div className="workspaceFilesHint is-error">{state.error}</div>
        ) : null}
        {!workspaceRoot && !state.loading && !state.error ? (
          <div className="workspaceFilesEmpty">{t("workspaceFilesPanel.noWorkspaceOpen")}</div>
        ) : null}
        {state.tree ? (
          <>
            <div className="workspaceFilesRoot" aria-label={state.tree.root}>
              {state.tree.root}
            </div>
            <div className="workspaceFilesTree">
              {state.tree.entries.map((entry) =>
                renderFileEntry(
                  entry,
                  onOpenFile,
                  normalizedFocusedPath,
                  selectedFileRef,
                ),
              )}
            </div>
          </>
        ) : null}
      </div>
    </section>
  );
}

export default WorkspaceFilesPanel;
