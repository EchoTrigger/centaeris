import { apiUrl } from "../api";

export function WorkspaceAvatar({ workspace, className = "shWsAvatar" }: Readonly<{
  workspace: { name: string; avatarUrl: string | null };
  className?: string;
}>) {
  return <span className={className} data-has-avatar={Boolean(workspace.avatarUrl)} aria-hidden="true">{workspace.avatarUrl
    ? <img src={workspace.avatarUrl.startsWith("data:") ? workspace.avatarUrl : apiUrl(workspace.avatarUrl)} alt="" />
    : Array.from(workspace.name)[0]}</span>;
}
