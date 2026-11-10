import { createStore } from "zustand/vanilla";
import type { WorkspaceGitHubCliStatusResponse, WorkspaceGitStatusResponse, WorkspaceSnapshot } from "../../lib/workspaceBridge";

export type WorkspaceCatalogErrorState = {
  message: string;
  canReset: boolean;
};
type WorkspaceCatalogViewState = {
  snapshot: WorkspaceSnapshot;
  catalogError: WorkspaceCatalogErrorState | null;
  gitStatus: WorkspaceGitStatusResponse | null;
  gitStatusError: string;
  githubCliStatus: WorkspaceGitHubCliStatusResponse | null;
  commitSnapshot: (snapshot: WorkspaceSnapshot) => void;
  setCatalogError: (error: WorkspaceCatalogErrorState) => void;
  setGitStatus: (status: WorkspaceGitStatusResponse | null) => void;
  setGitStatusError: (error: string) => void;
  setGithubCliStatus: (status: WorkspaceGitHubCliStatusResponse | null) => void;
};

export function createWorkspaceCatalogViewStore() {
  return createStore<WorkspaceCatalogViewState>()(set => ({
    snapshot: { activeWorkspaceRoot: null, workspaces: [], cancelled: false },
    catalogError: null,
    gitStatus: null,
    gitStatusError: "",
    githubCliStatus: null,
    commitSnapshot: snapshot => set({ snapshot, catalogError: null }),
    setCatalogError: catalogError => set({ catalogError }),
    setGitStatus: gitStatus => set({ gitStatus }),
    setGitStatusError: gitStatusError => set({ gitStatusError }),
    setGithubCliStatus: githubCliStatus => set({ githubCliStatus }),
  }));
}
