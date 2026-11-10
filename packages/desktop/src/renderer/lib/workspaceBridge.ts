import { t } from "../i18n";
import { invokeHost, isNativeHostRuntime } from "../host/hostBridge";
import { requireDevHostMock } from "./devHostMock";

export type WorkspaceFileTreeEntry = {
  name: string;
  path: string;
  isDirectory: boolean;
  children: WorkspaceFileTreeEntry[];
};

export type WorkspaceFileTreeResponse = {
  root: string;
  entries: WorkspaceFileTreeEntry[];
  truncated: boolean;
};

export type FilePreviewContentKind = "text" | "image" | "pdf";

export type WorkspaceReadFileResponse = {
  root: string;
  path: string;
  name: string;
  content: string;
  byteLen: number;
  encoding: string;
  contentKind: Exclude<FilePreviewContentKind, "pdf">;
  mimeType?: string;
  dataUrl?: string;
};

export type DesktopFilePreviewReadResponse = {
  root: string;
  path: string;
  name: string;
  content: string;
  byteLen: number;
  encoding: string;
  contentKind: FilePreviewContentKind;
  mimeType?: string;
  dataUrl?: string;
};

type DesktopFilePreviewReadOptions = {
  workspaceRoot?: string;
  basePath?: string;
};

export type WorkspaceInfo = {
  root: string;
  name: string;
  activeSessionId?: string;
  sortOrder: number;
  updatedAt: number;
};

export type WorkspaceSnapshot = {
  activeWorkspaceRoot?: string | null;
  workspaces: WorkspaceInfo[];
  cancelled: boolean;
};

export type WorkspaceOpenMode = "defaultDirectory" | "customPath";

export type WorkspaceRemoveResponse = {
  removed: boolean;
};

export type WorkspaceCatalogResetResponse = {
  snapshot: WorkspaceSnapshot;
  quarantinedPath: string;
};

export type WorkspaceGitChangedFile = {
  path: string;
  status: string;
  added: number;
  removed: number;
  diffAvailable?: boolean;
  diffUnavailableReason?: string | null;
};

export type WorkspaceGitStatusResponse = {
  workspaceRoot: string;
  branch?: string | null;
  changedFiles: WorkspaceGitChangedFile[];
  totalAdded: number;
  totalRemoved: number;
  isGitRepository: boolean;
};

export type WorkspaceGitDiffResponse = {
  workspaceRoot: string;
  diffPreview: string;
  truncated: boolean;
};

export type WorkspaceGitFileDiffResponse = {
  workspaceRoot: string;
  path: string;
  diffPreview: string;
  truncated: boolean;
};

export type WorkspaceGitHubCliStatusResponse = {
  available: boolean;
  summary: string;
};

const mockWorkspaceTree: WorkspaceFileTreeResponse = {
  root: "D:\\Projects\\Centaeris",
  truncated: false,
  entries: [
    { name: "core", path: "core", isDirectory: true, children: [] },
    { name: "desktop", path: "desktop", isDirectory: true, children: [] },
    { name: "docs", path: "docs", isDirectory: true, children: [] },
    { name: "ui", path: "ui", isDirectory: true, children: [] },
    { name: "AGENTS.md", path: "AGENTS.md", isDirectory: false, children: [] },
    { name: "README.md", path: "README.md", isDirectory: false, children: [] },
  ],
};

const mockWorkspaceInfo: WorkspaceInfo = {
  root: mockWorkspaceTree.root,
  name: "Centaeris",
  sortOrder: 0,
  updatedAt: Date.now(),
};

const mockWorkspaceSnapshot: WorkspaceSnapshot = {
  activeWorkspaceRoot: mockWorkspaceInfo.root,
  workspaces: [mockWorkspaceInfo],
  cancelled: false,
};

const mockWorkspaceFiles: Record<string, string> = {
  "AGENTS.md": t("workspaceBridge.agentsMdExampleLocalDevelopmentConventions"),
  "README.md": "# Centaeris\n\nWorkspace file preview is available in desktop mode.",
};

const shouldUseWorkspaceMock = (capability: string): boolean => {
  if (isNativeHostRuntime()) {
    return false;
  }
  requireDevHostMock(capability);
  return true;
};

export const getWorkspaceInfo = async (): Promise<WorkspaceSnapshot> => {
  if (shouldUseWorkspaceMock("workspace_get")) {
    return mockWorkspaceSnapshot;
  }
  return invokeHost<WorkspaceSnapshot>("workspace_get", {});
};

export const openWorkspaceFolder = async (mode: WorkspaceOpenMode): Promise<WorkspaceSnapshot> => {
  if (shouldUseWorkspaceMock("workspace_open_folder")) {
    return mockWorkspaceSnapshot;
  }
  return invokeHost<WorkspaceSnapshot>("workspace_open_folder", {
    request: { mode },
  });
};

export const activateWorkspaceRoot = async (root: string): Promise<WorkspaceSnapshot> => {
  if (shouldUseWorkspaceMock("workspace_activate")) {
    return {
      ...mockWorkspaceSnapshot,
      activeWorkspaceRoot: root,
    };
  }
  return invokeHost<WorkspaceSnapshot>("workspace_activate", {
    request: {
      root,
    },
  });
};

export const revealWorkspaceFolder = async (root: string): Promise<WorkspaceSnapshot> => {
  if (shouldUseWorkspaceMock("workspace_reveal_folder")) {
    return mockWorkspaceSnapshot;
  }
  return invokeHost<WorkspaceSnapshot>("workspace_reveal_folder", {
    request: {
      root,
    },
  });
};

export const renameWorkspace = async (root: string, name: string): Promise<WorkspaceSnapshot> => {
  if (shouldUseWorkspaceMock("workspace_rename")) {
    return {
      ...mockWorkspaceSnapshot,
      workspaces: mockWorkspaceSnapshot.workspaces.map((workspace) =>
        workspace.root === root ? { ...workspace, name } : workspace,
      ),
    };
  }
  return invokeHost<WorkspaceSnapshot>("workspace_rename", {
    request: {
      root,
      name,
    },
  });
};

export const removeWorkspace = async (root: string): Promise<WorkspaceRemoveResponse> => {
  if (shouldUseWorkspaceMock("workspace_remove")) {
    return { removed: true };
  }
  return invokeHost<WorkspaceRemoveResponse>("workspace_remove", {
    request: {
      root,
    },
  });
};

export const resetWorkspaceCatalog = async (): Promise<WorkspaceCatalogResetResponse> => {
  if (shouldUseWorkspaceMock("workspace_reset")) {
    throw new Error("workspace_reset is only available in the native host");
  }
  return invokeHost<WorkspaceCatalogResetResponse>("workspace_reset", {
    request: { confirm: true },
  });
};

export const getWorkspaceFileTree = async (
  maxDepth = 12,
  sessionId?: string,
  workspaceRoot?: string,
): Promise<WorkspaceFileTreeResponse> => {
  if (shouldUseWorkspaceMock("workspace_file_tree")) {
    return mockWorkspaceTree;
  }
  return invokeHost<WorkspaceFileTreeResponse>("workspace_file_tree", {
    request: {
      sessionId,
      workspaceRoot,
      maxDepth,
    },
  });
};

export const readWorkspaceFile = async (
  path: string,
  sessionId?: string,
  workspaceRoot?: string,
  agentRunId?: string,
): Promise<WorkspaceReadFileResponse> => {
  if (shouldUseWorkspaceMock("workspace_read_file")) {
    const normalizedPath = path.replace(/\\/g, "/");
    const name = normalizedPath.split("/").filter(Boolean).at(-1) || normalizedPath;
    const content = mockWorkspaceFiles[normalizedPath] ?? `// Mock preview for ${normalizedPath}\n`;
    return {
      root: mockWorkspaceTree.root,
      path: normalizedPath,
      name,
      content,
      byteLen: new TextEncoder().encode(content).length,
      encoding: "utf-8",
      contentKind: "text",
      mimeType: "text/plain; charset=utf-8",
    };
  }
  return invokeHost<WorkspaceReadFileResponse>("workspace_read_file", {
    request: {
      sessionId,
      workspaceRoot,
      agentRunId,
      path,
    },
  });
};

export const readDesktopFilePreview = async (
  path: string,
  options: DesktopFilePreviewReadOptions = {},
): Promise<DesktopFilePreviewReadResponse> => {
  if (shouldUseWorkspaceMock("desktop_file_preview_read")) {
    const normalizedPath = path.replace(/\\/g, "/");
    const name = normalizedPath.split("/").filter(Boolean).at(-1) || normalizedPath;
    const content =
      mockWorkspaceFiles[normalizedPath] ?? `// Mock desktop preview for ${normalizedPath}\n`;
    return {
      root: options.basePath ?? options.workspaceRoot ?? mockWorkspaceTree.root,
      path: normalizedPath,
      name,
      content,
      byteLen: new TextEncoder().encode(content).length,
      encoding: "utf-8",
      contentKind: "text",
      mimeType: "text/plain; charset=utf-8",
    };
  }
  return invokeHost<DesktopFilePreviewReadResponse>("desktop_file_preview_read", {
    request: {
      path,
      workspaceRoot: options.workspaceRoot,
      basePath: options.basePath,
    },
  });
};

const requireNativeGitWorkbench = (capability: string): void => {
  if (!isNativeHostRuntime()) {
    throw new Error(`${capability} is desktop-only in Rust mainline`);
  }
};

export const getWorkspaceGitStatus = async (
  workspaceRoot: string,
): Promise<WorkspaceGitStatusResponse> => {
  requireNativeGitWorkbench("workspace git status");
  return invokeHost<WorkspaceGitStatusResponse>("workspace_git_status_get", {
    request: {
      workspaceRoot,
    },
  });
};

export const getWorkspaceGitDiff = async (
  workspaceRoot: string,
): Promise<WorkspaceGitDiffResponse> => {
  requireNativeGitWorkbench("workspace git diff");
  return invokeHost<WorkspaceGitDiffResponse>("workspace_git_diff_get", {
    request: {
      workspaceRoot,
    },
  });
};

export const getWorkspaceGitFileDiff = async (
  workspaceRoot: string,
  path: string,
): Promise<WorkspaceGitFileDiffResponse> => {
  requireNativeGitWorkbench("workspace git file diff");
  return invokeHost<WorkspaceGitFileDiffResponse>("workspace_git_file_diff_get", {
    request: {
      workspaceRoot,
      path,
    },
  });
};

export const getWorkspaceGitHubCliStatus = async (): Promise<WorkspaceGitHubCliStatusResponse> => {
  requireNativeGitWorkbench("workspace GitHub CLI status");
  return invokeHost<WorkspaceGitHubCliStatusResponse>("workspace_git_github_cli_status_get", {});
};

export type WorkspaceGitSnapshot = { head: string | null; indexFingerprint: string };
export type WorkspaceGitReviewFile = { path: string; originalPath: string | null; status: string };
export type WorkspaceGitReviewResponse = {
  workspaceRoot: string;
  branch: string | null;
  snapshot: WorkspaceGitSnapshot;
  staged: WorkspaceGitReviewFile[];
  unstaged: WorkspaceGitReviewFile[];
  untracked: WorkspaceGitReviewFile[];
  conflicts: WorkspaceGitReviewFile[];
};
export const getWorkspaceGitReview = (
  workspaceRoot: string,
): Promise<WorkspaceGitReviewResponse> => {
  requireNativeGitWorkbench("workspace Git review");
  return invokeHost("workspace_git_review_get", { request: { workspaceRoot } });
};
export const getWorkspaceGitReviewDiff = (
  workspaceRoot: string,
  path: string,
  side: "staged" | "unstaged",
): Promise<WorkspaceGitFileDiffResponse> => {
  requireNativeGitWorkbench("workspace Git review diff");
  return invokeHost("workspace_git_review_diff_get", { request: { workspaceRoot, path, side } });
};
export const stageWorkspaceGitFile = (
  workspaceRoot: string,
  path: string,
  expected: WorkspaceGitSnapshot,
): Promise<WorkspaceGitReviewResponse> => {
  requireNativeGitWorkbench("workspace Git stage");
  return invokeHost("workspace_git_stage", { request: { workspaceRoot, path, expected } });
};
export const unstageWorkspaceGitFile = (
  workspaceRoot: string,
  path: string,
  expected: WorkspaceGitSnapshot,
): Promise<WorkspaceGitReviewResponse> => {
  requireNativeGitWorkbench("workspace Git unstage");
  return invokeHost("workspace_git_unstage", { request: { workspaceRoot, path, expected } });
};
export const commitWorkspaceGit = (
  workspaceRoot: string,
  message: string,
  expected: WorkspaceGitSnapshot,
): Promise<{ commitSha: string; output: string }> => {
  requireNativeGitWorkbench("workspace Git commit");
  return invokeHost("workspace_git_commit", { request: { workspaceRoot, message, expected } });
};

export type WorkspaceGitSource = "unstaged" | "staged" | "branch";
export type WorkspaceGitViewFile = WorkspaceGitReviewFile & {
  added: number | null;
  removed: number | null;
};
export type WorkspaceGitView = {
  workspaceRoot: string;
  branch: string | null;
  source: WorkspaceGitSource;
  snapshot: WorkspaceGitSnapshot;
  baseRef: string | null;
  files: WorkspaceGitViewFile[];
  hasConflicts: boolean;
  diff: WorkspaceGitFileDiffResponse | null;
};
export const getWorkspaceGitView = (
  workspaceRoot: string,
  source: WorkspaceGitSource,
  baseRef?: string,
  path?: string,
): Promise<WorkspaceGitView> => {
  requireNativeGitWorkbench("workspace Git view");
  return invokeHost("workspace_git_view_get", {
    request: { workspaceRoot, source, baseRef, path },
  });
};
