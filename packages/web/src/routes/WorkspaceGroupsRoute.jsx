import { t } from "../i18n";
import { useTranslation } from "../i18n";
import { useCallback, useEffect, useState } from "react";
import { Plus, ShieldCheck, Trash2, UsersRound } from "lucide-react";
import { Link, Navigate, useNavigate, useRouteLoaderData } from "react-router";
import { ApiError, apiJson, jsonOptions } from "../api";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { ShellPage } from "../shell/ShellPage";
import { redirectAfterWorkspaceNotFound } from "../workspaceAccess";

const ADMIN_ROLES = new Set(["owner", "admin"]);
const ERRORS = () => ({
  workspace_group_name_exists: t("workspaceGroupsRoute.aGroupWithThisNameAlreadyExistsInThis"),
  workspace_group_name_unchanged: t("workspaceGroupsRoute.theGroupNameHasNotChanged"),
  workspace_system_group_immutable: t("workspaceGroupsRoute.systemDynamicGroupsCannotBeModified"),
  workspace_group_not_found: t("workspaceGroupsRoute.thisGroupNoLongerExists"),
  workspace_member_not_found: t("workspaceGroupsRoute.membershipHasChanged"),
});

function errorText(error) {
  const message = error instanceof Error ? error.message : String(error);
  return ERRORS()[message] || message;
}

function roleLabel(role) {
  return { owner: t("workspaceChooserRoute.owner"), admin: t("invitationActivationRoute.administrator"), member: t("invitationActivationRoute.member") }[role] || role;
}

export default function WorkspaceGroupsRoute({ embedded = false } = {}) {
  const { t } = useTranslation();
  const { workspace } = useRouteLoaderData("workspace");
  const navigate = useNavigate();
  const base = `/w/${encodeURIComponent(workspace.id)}`;
  const canManageWorkspace = ADMIN_ROLES.has(workspace.role);
  const [groups, setGroups] = useState(null);
  const [members, setMembers] = useState(null);
  const [selectedId, setSelectedId] = useState("");
  const [groupMemberIds, setGroupMemberIds] = useState(new Set());
  const [loading, setLoading] = useState(true);
  const [rosterLoading, setRosterLoading] = useState(false);
  const [pageError, setPageError] = useState("");
  const [notice, setNotice] = useState(null);
  const [creating, setCreating] = useState(false);
  const [createName, setCreateName] = useState("");
  const [renameName, setRenameName] = useState("");
  const [renaming, setRenaming] = useState(false);
  const [busyKeys, setBusyKeys] = useState(() => new Set());
  const [deleteTarget, setDeleteTarget] = useState(null);

  function setBusy(key, busy) {
    setBusyKeys((current) => {
      const next = new Set(current);
      if (busy) next.add(key);
      else next.delete(key);
      return next;
    });
  }

  const lostAccess = useCallback(async () => {
    await redirectAfterWorkspaceNotFound(workspace, navigate);
  }, [navigate, workspace]);

  const load = useCallback(async () => {
    setLoading(true);
    setPageError("");
    try {
      const [groupResult, memberResult] = await Promise.all([
        apiJson(`/api/workspaces/${encodeURIComponent(workspace.id)}/groups`),
        apiJson(`/api/workspaces/${encodeURIComponent(workspace.id)}/members`),
      ]);
      if (!Array.isArray(groupResult.groups) || !Array.isArray(memberResult.members)) throw new Error("workspace_groups_invalid");
      const nextGroups = groupResult.groups;
      setGroups(nextGroups);
      setMembers(memberResult.members);
      setSelectedId((current) => nextGroups.some((group) => group.id === current) ? current : (nextGroups[0]?.id || ""));
    } catch (error) {
      if (error instanceof ApiError && error.message === "workspace_not_found") await lostAccess();
      else setPageError(errorText(error));
    } finally {
      setLoading(false);
    }
  }, [lostAccess, workspace.id]);

  useEffect(() => {
    if (canManageWorkspace) void load();
  }, [canManageWorkspace, load]);

  useEffect(() => {
    if (!selectedId) {
      setGroupMemberIds(new Set());
      return undefined;
    }
    let active = true;
    setRosterLoading(true);
    apiJson(`/api/workspaces/${encodeURIComponent(workspace.id)}/groups/${encodeURIComponent(selectedId)}/members`)
      .then((result) => {
        if (active) {
          if (!Array.isArray(result.members)) throw new Error("workspace_group_members_invalid");
          setGroupMemberIds(new Set(result.members.map((member) => member.membershipId)));
        }
      })
      .catch(async (error) => {
        if (!active) return;
        if (error instanceof ApiError && error.message === "workspace_not_found") await lostAccess();
        else {
          setNotice({ kind: "error", text: errorText(error) });
          if (error instanceof ApiError && error.message === "workspace_group_not_found") await load();
        }
      })
      .finally(() => { if (active) setRosterLoading(false); });
    return () => { active = false; };
  }, [load, lostAccess, selectedId, workspace.id]);

  useEffect(() => {
    if (!notice) return undefined;
    const timeout = window.setTimeout(() => setNotice(null), 4200);
    return () => window.clearTimeout(timeout);
  }, [notice]);

  async function handleError(error) {
    if (error instanceof ApiError && error.message === "workspace_not_found") {
      await lostAccess();
      return;
    }
    if (error instanceof ApiError && [404, 409].includes(error.status)) await load();
    setNotice({ kind: "error", text: errorText(error) });
  }

  async function createGroup(event) {
    event.preventDefault();
    const key = "create";
    setBusy(key, true);
    try {
      const result = await apiJson(`/api/workspaces/${encodeURIComponent(workspace.id)}/groups`, jsonOptions("POST", { name: createName }));
      const group = result.group;
      setGroups((items) => [...items, group]);
      setSelectedId(group.id);
      setCreateName("");
      setCreating(false);
      setNotice({ kind: "success", text: t("workspaceGroupsRoute.createdGroupValue", { value1: group.name }) });
    } catch (error) {
      await handleError(error);
    } finally {
      setBusy(key, false);
    }
  }

  async function renameGroup(event) {
    event.preventDefault();
    const selected = groups.find((group) => group.id === selectedId);
    if (!selected) return;
    const key = `rename:${selected.id}`;
    setBusy(key, true);
    try {
      const result = await apiJson(`/api/workspaces/${encodeURIComponent(workspace.id)}/groups/${encodeURIComponent(selected.id)}`, jsonOptions("PATCH", { name: renameName }));
      const group = result.group;
      setGroups((items) => items.map((item) => item.id === group.id ? group : item));
      setRenaming(false);
      setNotice({ kind: "success", text: t("workspaceGroupsRoute.groupRenamedToValue", { value1: group.name }) });
    } catch (error) {
      await handleError(error);
    } finally {
      setBusy(key, false);
    }
  }

  async function toggleMember(member) {
    const group = groups.find((item) => item.id === selectedId);
    if (!group || group.kind !== "custom") return;
    const included = groupMemberIds.has(member.membershipId);
    const key = `member:${member.membershipId}`;
    setBusy(key, true);
    try {
      const result = await apiJson(
        `/api/workspaces/${encodeURIComponent(workspace.id)}/groups/${encodeURIComponent(group.id)}/members/${encodeURIComponent(member.membershipId)}`,
        { method: included ? "DELETE" : "PUT" },
      );
      if (result.ok !== true) throw new Error("workspace_group_member_result_invalid");
      setGroupMemberIds((current) => {
        const next = new Set(current);
        if (included) next.delete(member.membershipId);
        else next.add(member.membershipId);
        return next;
      });
    } catch (error) {
      await handleError(error);
    } finally {
      setBusy(key, false);
    }
  }

  async function deleteGroup() {
    if (!deleteTarget) return;
    const target = deleteTarget;
    const key = `delete:${target.id}`;
    setBusy(key, true);
    try {
      const result = await apiJson(`/api/workspaces/${encodeURIComponent(workspace.id)}/groups/${encodeURIComponent(target.id)}`, { method: "DELETE" });
      if (result.ok !== true) throw new Error("workspace_group_delete_result_invalid");
      const remaining = groups.filter((group) => group.id !== target.id);
      setGroups(remaining);
      setSelectedId(remaining[0]?.id || "");
      setDeleteTarget(null);
      setNotice({ kind: "success", text: t("workspaceGroupsRoute.valueWasDeleted", { value1: target.name }) });
    } catch (error) {
      await handleError(error);
    } finally {
      setBusy(key, false);
    }
  }

  if (!canManageWorkspace) return <Navigate replace to={`${base}/app`} state={{ workspaceNotice: t("settingsRoute.youDoNotHaveAccessToWorkspaceSettings") }} />;
  const selected = groups?.find((group) => group.id === selectedId) || null;
  const deleteBusy = deleteTarget ? busyKeys.has(`delete:${deleteTarget.id}`) : false;

  const content = <>
    <div className={`shWorkspaceSettingsPage ${embedded ? "isEmbedded" : ""}`}>
      <header className="shWorkspaceSettingsHeader"><div><span>{workspace.name}{" "}{t("workspaceGroupsRoute.workspaceSettings")}</span><h1>{t("settingsRoute.groups")}</h1></div></header>
      <nav className="shWorkspaceSettingsTabs" aria-label={t("workspaceGroupsRoute.permissionSettings")}>
        <Link to={`${base}/settings/members`}>{t("workspaceGroupsRoute.membersAndInvitations")}</Link>
        <span className="isActive" aria-current="page">{t("settingsRoute.groups")}</span>
      </nav>

      {loading ? <div className="shWorkspaceSettingsState" aria-live="polite">{t("workspaceGroupsRoute.loadingGroups")}</div> : null}
      {!loading && pageError ? <div className="shWorkspaceSettingsState isError" role="alert">{t("workspaceGroupsRoute.unableToLoadGroups")}{pageError}<button type="button" onClick={() => void load()}>{t("router.reloadPage")}</button></div> : null}
      {!loading && !pageError && groups && members ? <div className="shWorkspaceGroupLayout">
        <aside aria-label={t("workspaceGroupsRoute.groupList")}>
          <header><strong>{t("settingsRoute.groups")}</strong><button type="button" aria-label={t("workspaceGroupsRoute.newGroup")} onClick={() => setCreating(true)}><Plus aria-hidden="true" /></button></header>
          {creating ? <form onSubmit={createGroup}><input autoFocus maxLength={160} required aria-label={t("workspaceGroupsRoute.newGroupName")} value={createName} onChange={(event) => setCreateName(event.target.value)} /><span><button type="button" onClick={() => { setCreating(false); setCreateName(""); }}>{t("agentRunRow.cancel")}</button><button type="submit" disabled={busyKeys.has("create") || !createName.trim()}>{t("libraryRoute.create")}</button></span></form> : null}
          <nav>{groups.map((group) => <button className={group.id === selectedId ? "isActive" : ""} type="button" key={group.id} onClick={() => { setSelectedId(group.id); setRenaming(false); }}><span>{group.kind === "all_members" ? <ShieldCheck aria-hidden="true" /> : <UsersRound aria-hidden="true" />}{group.name}</span><small>{group.kind === "all_members" ? t("libraryRoute.system") : t("workspaceGroupsRoute.custom")}</small></button>)}</nav>
        </aside>

        <section className="shWorkspaceGroupDetail">
          {selected ? <>
            <header>
              <div>{renaming ? <form onSubmit={renameGroup}><input autoFocus maxLength={160} required aria-label={t("workspaceGroupsRoute.groupName")} value={renameName} onChange={(event) => setRenameName(event.target.value)} /><button type="button" onClick={() => setRenaming(false)}>{t("agentRunRow.cancel")}</button><button type="submit" disabled={busyKeys.has(`rename:${selected.id}`) || !renameName.trim()}>{t("modelSettings.save")}</button></form> : <><h2>{selected.name}</h2><p>{selected.kind === "all_members" ? t("workspaceGroupsRoute.automaticallyIncludesAllCurrentlyActiveMembers") : t("workspaceGroupsRoute.valueMembers", { value1: groupMemberIds.size })}</p></>}</div>
              {selected.kind === "custom" && !renaming ? <span><button type="button" onClick={() => { setRenameName(selected.name); setRenaming(true); }}>{t("appRoute.rename")}</button><button className="isDanger" type="button" aria-label={t("workspaceGroupsRoute.deleteValue", { value1: selected.name })} onClick={() => setDeleteTarget(selected)}><Trash2 aria-hidden="true" />{t("appRoute.delete")}</button></span> : null}
            </header>
            {rosterLoading ? <div className="shWorkspaceSettingsState">{t("workspaceGroupsRoute.loadingGroupMembers")}</div> : <div className="shWorkspaceGroupMembers">
              {members.map((member) => {
                const checked = selected.kind === "all_members" || groupMemberIds.has(member.membershipId);
                const rowBusy = busyKeys.has(`member:${member.membershipId}`);
                return <label key={member.membershipId}><input type="checkbox" checked={checked} disabled={selected.kind === "all_members" || rowBusy} onChange={() => void toggleMember(member)} /><span className="shWorkspaceMemberAvatar" aria-hidden="true">{member.email.slice(0, 1).toUpperCase()}</span><span><strong>{member.email}</strong><small>{roleLabel(member.role)}</small></span></label>;
              })}
            </div>}
          </> : <div className="shWorkspaceSettingsEmpty">{t("workspaceGroupsRoute.noGroupsYet")}</div>}
        </section>
      </div> : null}
    </div>
    {notice ? <div className={`shWorkspaceToast ${notice.kind === "error" ? "isError" : ""}`} role={notice.kind === "error" ? "alert" : "status"}>{notice.text}</div> : null}
    <ConfirmDialog open={Boolean(deleteTarget)} title={t("workspaceGroupsRoute.deleteGroup")} message={deleteTarget ? t("workspaceGroupsRoute.deleteValueCurrentMembershipsAndAllSourceGrantsWill", { value1: deleteTarget.name }) : ""} confirmLabel={t("appRoute.delete")} busy={deleteBusy} onCancel={() => setDeleteTarget(null)} onConfirm={() => void deleteGroup()} />
  </>;
  return embedded ? content : <ShellPage>{content}</ShellPage>;
}
