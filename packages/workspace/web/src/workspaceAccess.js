import { t } from "./i18n";
import { apiJson, requireWorkspaces } from "./api";

export async function redirectAfterWorkspaceNotFound(workspace, navigate) {
  let destination = "/workspaces";
  let message = t("workspaceAccess.youAreNoLongerAMemberOfValue", { value1: workspace.name });
  try {
    const result = await apiJson("/api/workspaces");
    if (requireWorkspaces(result.workspaces).some((item) => item.id === workspace.id)) {
      destination = `/w/${encodeURIComponent(workspace.id)}/app`;
      message = t("workspaceAccess.youNoLongerHaveAccessToWorkspaceSettings");
    }
  } catch {
    // The chooser performs its own authenticated reload and remains the safe destination.
  }
  navigate(destination, { replace: true, state: { workspaceNotice: message } });
}
