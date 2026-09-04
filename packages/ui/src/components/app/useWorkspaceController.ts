import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ConfirmAction } from "../ConfirmDialog";
import {
  activateWorkspaceRoot,
  getWorkspaceGitHubCliStatus,
  getWorkspaceGitStatus,
  getWorkspaceInfo,
  openWorkspaceFolder,
  resetWorkspaceCatalog,
  type WorkspaceGitHubCliStatusResponse,
  type WorkspaceGitStatusResponse,
  type WorkspaceOpenMode,
  type WorkspaceSnapshot,
} from "../../lib/workspaceBridge";

export type WorkspaceCatalogErrorState = {
  message: string;
  canReset: boolean;
};

const emptyWorkspaceSnapshot: WorkspaceSnapshot = {
  activeWorkspaceRoot: null,
  workspaces: [],
  cancelled: false,
};

const normalizeRoot = (root?: string | null): string =>
  String(root || "")
    .trim()
    .replace(/^\\\\\?\\/, "")
    .replace(/\\/g, "/")
    .replace(/\/+$/, "")
    .toLowerCase();

const errorMessage = (error: unknown, fallback: string): string =>
  error instanceof Error && error.message.trim()
    ? error.message
    : typeof error === "string" && error.trim()
      ? error
      : fallback;

export function useWorkspaceController({
  confirmAction,
  reportHostError,
}: {
  confirmAction: ConfirmAction;
  reportHostError: (message: string) => void;
}) {
  const [snapshot, setSnapshot] = useState<WorkspaceSnapshot>(emptyWorkspaceSnapshot);
  const [catalogError, setCatalogError] = useState<WorkspaceCatalogErrorState | null>(null);
  const [gitStatus, setGitStatus] = useState<WorkspaceGitStatusResponse | null>(null);
  const [gitStatusError, setGitStatusError] = useState("");
  const [githubCliStatus, setGithubCliStatus] = useState<WorkspaceGitHubCliStatusResponse | null>(null);
  const inputsRef = useRef({ confirmAction, reportHostError });
  const actionRevisionRef = useRef(0);
  inputsRef.current = { confirmAction, reportHostError };

  const commitSnapshot = useCallback((nextSnapshot: WorkspaceSnapshot) => {
    setSnapshot(nextSnapshot);
    setCatalogError(null);
  }, []);

  const beginAction = useCallback((): number => {
    const revision = actionRevisionRef.current + 1;
    actionRevisionRef.current = revision;
    return revision;
  }, []);

  const isActionOwner = useCallback(
    (revision: number): boolean => actionRevisionRef.current === revision,
    [],
  );

  const applyOwnedSnapshot = useCallback((
    nextSnapshot: WorkspaceSnapshot,
    revision: number,
  ): boolean => {
    if (!isActionOwner(revision)) return false;
    commitSnapshot(nextSnapshot);
    return true;
  }, [commitSnapshot, isActionOwner]);

  const applySnapshot = useCallback((nextSnapshot: WorkspaceSnapshot) => {
    actionRevisionRef.current += 1;
    commitSnapshot(nextSnapshot);
  }, [commitSnapshot]);

  const beginInitialization = useCallback(() => {
    const ownerRevision = actionRevisionRef.current;
    const isCurrent = (): boolean => actionRevisionRef.current === ownerRevision;
    return {
      isCurrent,
      applySnapshot: (nextSnapshot: WorkspaceSnapshot): boolean => {
        if (!isCurrent()) return false;
        commitSnapshot(nextSnapshot);
        return true;
      },
    };
  }, [commitSnapshot]);

  const reportCatalogFailure = useCallback((error: unknown, fallback: string) => {
    const message = errorMessage(error, fallback);
    if (message.includes("workspace_catalog_")) {
      setCatalogError({
        message,
        canReset: message.includes("workspace_catalog_corrupt"),
      });
      return;
    }
    inputsRef.current.reportHostError(message);
  }, []);

  const retryCatalog = useCallback(async () => {
    const revision = beginAction();
    try {
      applyOwnedSnapshot(await getWorkspaceInfo(), revision);
    } catch (error) {
      if (isActionOwner(revision)) {
        reportCatalogFailure(error, "重新加载工作区列表失败");
      }
    }
  }, [applyOwnedSnapshot, beginAction, isActionOwner, reportCatalogFailure]);

  const resetCatalog = useCallback(async (): Promise<boolean> => {
    let revision: number | null = null;
    try {
      const confirmed = await inputsRef.current.confirmAction({
        title: "Reset the workspace list?",
        message: "This removes the saved workspace list. Project files stay unchanged.",
      });
      if (!confirmed) return false;
      // Showing a confirmation is not a workspace mutation. Take ownership only
      // after approval so cancelling the dialog cannot invalidate pending work.
      revision = beginAction();
      const response = await resetWorkspaceCatalog();
      return applyOwnedSnapshot(response.snapshot, revision);
    } catch (error) {
      if (revision === null || isActionOwner(revision)) {
        reportCatalogFailure(error, "重置工作区列表失败");
      }
      return false;
    }
  }, [applyOwnedSnapshot, beginAction, isActionOwner, reportCatalogFailure]);

  const openWorkspace = useCallback(async (mode: WorkspaceOpenMode): Promise<boolean> => {
    const revision = beginAction();
    try {
      const nextSnapshot = await openWorkspaceFolder(mode);
      if (nextSnapshot.cancelled) return false;
      return applyOwnedSnapshot(nextSnapshot, revision);
    } catch (error) {
      if (isActionOwner(revision)) {
        reportCatalogFailure(error, "打开工作区失败");
      }
      return false;
    }
  }, [applyOwnedSnapshot, beginAction, isActionOwner, reportCatalogFailure]);

  const selectWorkspace = useCallback(async (root: string): Promise<boolean> => {
    const revision = beginAction();
    try {
      return applyOwnedSnapshot(await activateWorkspaceRoot(root), revision);
    } catch (error) {
      if (isActionOwner(revision)) {
        reportCatalogFailure(error, "切换工作区失败");
      }
      return false;
    }
  }, [applyOwnedSnapshot, beginAction, isActionOwner, reportCatalogFailure]);

  const activeWorkspaceRoot = snapshot.activeWorkspaceRoot ?? null;

  useEffect(() => {
    if (!activeWorkspaceRoot) {
      setGitStatus(null);
      setGithubCliStatus(null);
      setGitStatusError("");
      return;
    }
    let cancelled = false;
    void Promise.allSettled([
      getWorkspaceGitStatus(activeWorkspaceRoot),
      getWorkspaceGitHubCliStatus(),
    ]).then(([gitResult, githubResult]) => {
      if (cancelled) return;
      if (gitResult.status === "fulfilled") {
        setGitStatus(gitResult.value);
        setGitStatusError("");
      } else {
        setGitStatus(null);
        setGitStatusError(errorMessage(gitResult.reason, "Git 状态不可用"));
      }
      setGithubCliStatus(githubResult.status === "fulfilled" ? githubResult.value : null);
    });
    return () => {
      cancelled = true;
    };
  }, [activeWorkspaceRoot]);

  const activeWorkspace = useMemo(
    () => snapshot.workspaces.find(
      (workspace) => normalizeRoot(workspace.root) === normalizeRoot(activeWorkspaceRoot),
    ) ?? null,
    [activeWorkspaceRoot, snapshot.workspaces],
  );

  const actions = useMemo(() => ({
    applySnapshot,
    beginInitialization,
    openWorkspace,
    reportCatalogFailure,
    resetCatalog,
    retryCatalog,
    selectWorkspace,
  }), [
    applySnapshot,
    beginInitialization,
    openWorkspace,
    reportCatalogFailure,
    resetCatalog,
    retryCatalog,
    selectWorkspace,
  ]);

  return {
    snapshot,
    workspaces: snapshot.workspaces,
    activeWorkspaceRoot,
    activeWorkspace,
    catalogError,
    gitStatus,
    gitStatusError,
    githubCliStatus,
    actions,
  };
}
