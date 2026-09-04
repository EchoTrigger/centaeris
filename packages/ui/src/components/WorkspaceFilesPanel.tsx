import { type RefObject, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronRight, FileText, Image } from "lucide-react";
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
        aria-label={`打开 ${entry.path}`}
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
    setState({ tree: null, loading: true, error: "" });
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
            error instanceof Error ? error.message : "读取工作区文件失败。",
          loading: false,
        });
      });
    return () => {
      cancelled = true;
    };
  }, [sessionId, isOpen, workspaceRoot]);

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
        <button type="button" className="workspaceFilesScope" aria-label="所有文件">
          <span>所有文件</span>
          <ChevronDown className="workspaceFilesScopeIcon" aria-hidden="true" />
        </button>
        {focusedLine ? (
          <span className="workspaceFilesFocusLine">行 {focusedLine}</span>
        ) : null}
      </header>
      <div className="workspaceFilesBody">
        {state.loading ? (
          <div className="workspaceFilesHint">正在读取工作区...</div>
        ) : null}
        {state.error ? (
          <div className="workspaceFilesHint is-error">{state.error}</div>
        ) : null}
        {!workspaceRoot && !state.loading && !state.error ? (
          <div className="workspaceFilesEmpty">未打开工作区</div>
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
