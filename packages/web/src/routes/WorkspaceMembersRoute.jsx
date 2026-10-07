import { i18n } from "../i18n";
import { t } from "../i18n";
import { useTranslation } from "../i18n";
import { useCallback, useEffect, useState } from "react";
import { Copy, MoreHorizontal, UserPlus } from "lucide-react";
import { Link, Navigate, useNavigate, useRouteLoaderData } from "react-router";
import { ApiError, apiJson, jsonOptions } from "../api";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { WorkspaceDialog } from "../components/WorkspaceDialog";
import { ShellPage } from "../shell/ShellPage";
import { redirectAfterWorkspaceNotFound } from "../workspaceAccess";

const ADMIN_ROLES = new Set(["owner", "admin"]);

const ERROR_MESSAGES = () => ({
  workspace_member_exists: t("workspaceMembersRoute.thisAccountIsAlreadyAMemberOfThisWorkspace"),
  workspace_member_role_unchanged: t("workspaceMembersRoute.theMemberSRoleHasNotChanged"),
  workspace_member_self_operation_forbidden: t("workspaceMembersRoute.youCannotPerformThisActionOnYourOwnMembership"),
  workspace_owner_transfer_required: t("workspaceMembersRoute.theOwnerCanOnlyBeChangedThroughAnOwnership"),
  workspace_owner_reauthentication_failed: t("settingsRoute.theCurrentPasswordIsIncorrect"),
  workspace_invitation_not_pending: t("workspaceMembersRoute.theInvitationStatusHasChanged"),
});

function errorText(error) {
  const message = error instanceof Error ? error.message : String(error);
  return ERROR_MESSAGES()[message] || message;
}

function roleLabel(role) {
  return { owner: t("workspaceChooserRoute.owner"), admin: t("invitationActivationRoute.administrator"), member: t("invitationActivationRoute.member") }[role] || role;
}

function formatDate(value) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : new Intl.DateTimeFormat(i18n.resolvedLanguage, {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

export default function WorkspaceMembersRoute({ embedded = false } = {}) {
  const { t } = useTranslation();
  const { user } = useRouteLoaderData("authenticated");
  const { workspace } = useRouteLoaderData("workspace");
  const navigate = useNavigate();
  const base = `/w/${encodeURIComponent(workspace.id)}`;
  const canManageWorkspace = ADMIN_ROLES.has(workspace.role);
  const [members, setMembers] = useState(null);
  const [invitations, setInvitations] = useState(null);
  const [loading, setLoading] = useState(true);
  const [pageError, setPageError] = useState("");
  const [notice, setNotice] = useState(null);
  const [busyKeys, setBusyKeys] = useState(() => new Set());
  const [removeTarget, setRemoveTarget] = useState(null);
  const [transferTarget, setTransferTarget] = useState(null);
  const [transferPassword, setTransferPassword] = useState("");
  const [inviteDialog, setInviteDialog] = useState(null);
  const [reissueTarget, setReissueTarget] = useState(null);
  const [revokeTarget, setRevokeTarget] = useState(null);

  const redirectForLostAccess = useCallback(async () => {
    await redirectAfterWorkspaceNotFound(workspace, navigate);
  }, [navigate, workspace]);

  const load = useCallback(async (quiet = false) => {
    if (!quiet) setLoading(true);
    setPageError("");
    try {
      const [memberResult, invitationResult] = await Promise.all([
        apiJson(`/api/workspaces/${encodeURIComponent(workspace.id)}/members`),
        apiJson(`/api/workspaces/${encodeURIComponent(workspace.id)}/invitations`),
      ]);
      if (!Array.isArray(memberResult.members) || !Array.isArray(invitationResult.invitations)) throw new Error("workspace_members_invalid");
      setMembers(memberResult.members);
      setInvitations(invitationResult.invitations);
    } catch (error) {
      if (error instanceof ApiError && error.message === "workspace_not_found") {
        await redirectForLostAccess();
        return;
      }
      if (quiet) setNotice({ kind: "error", text: t("workspaceMembersRoute.unableToReloadValue", { value1: errorText(error) }) });
      else setPageError(errorText(error));
    } finally {
      if (!quiet) setLoading(false);
    }
  }, [redirectForLostAccess, workspace.id, t]);

  useEffect(() => {
    if (canManageWorkspace) void load();
  }, [canManageWorkspace, load]);

  useEffect(() => {
    if (!notice) return undefined;
    const timeout = window.setTimeout(() => setNotice(null), 4200);
    return () => window.clearTimeout(timeout);
  }, [notice]);

  function setBusy(key, busy) {
    setBusyKeys((current) => {
      const next = new Set(current);
      if (busy) next.add(key);
      else next.delete(key);
      return next;
    });
  }

  async function handleMutationError(error) {
    if (error instanceof ApiError && error.message === "workspace_not_found") {
      await redirectForLostAccess();
      return;
    }
    if (error instanceof ApiError && [404, 409].includes(error.status)) {
      await load(true);
      setNotice({ kind: "error", text: t("workspaceMembersRoute.theStatusChangedAndHasBeenReloadedValue", { value1: errorText(error) }) });
      return;
    }
    setNotice({ kind: "error", text: errorText(error) });
  }

  async function updateRole(member, role) {
    const key = `member:${member.membershipId}`;
    setBusy(key, true);
    try {
      const result = await apiJson(
        `/api/workspaces/${encodeURIComponent(workspace.id)}/members/${encodeURIComponent(member.membershipId)}`,
        jsonOptions("PATCH", { role }),
      );
      const updated = result.member;
      setMembers((items) => items.map((item) => item.membershipId === updated.membershipId ? updated : item));
      setNotice({ kind: "success", text: t("workspaceMembersRoute.valueIsNowValue", { value1: updated.email, value2: roleLabel(updated.role) }) });
    } catch (error) {
      await handleMutationError(error);
    } finally {
      setBusy(key, false);
    }
  }

  async function removeMember() {
    if (!removeTarget) return;
    const target = removeTarget;
    const key = `remove:${target.membershipId}`;
    setBusy(key, true);
    try {
      const result = await apiJson(
        `/api/workspaces/${encodeURIComponent(workspace.id)}/members/${encodeURIComponent(target.membershipId)}`,
        { method: "DELETE" },
      );
      if (result.ok !== true) throw new Error("workspace_member_remove_result_invalid");
      setMembers((items) => items.filter((item) => item.membershipId !== target.membershipId));
      setRemoveTarget(null);
      setNotice({ kind: "success", text: t("workspaceMembersRoute.valueWasRemovedFromTheWorkspace", { value1: target.email }) });
    } catch (error) {
      await handleMutationError(error);
    } finally {
      setBusy(key, false);
    }
  }

  async function transferOwnership(event) {
    event.preventDefault();
    if (!transferTarget || !transferPassword) return;
    const target = transferTarget;
    const key = `transfer:${target.membershipId}`;
    setBusy(key, true);
    try {
      const result = await apiJson(
        `/api/workspaces/${encodeURIComponent(workspace.id)}/owner-transfer`,
        jsonOptions("POST", {
          targetMembershipId: target.membershipId,
          currentPassword: transferPassword,
        }),
      );
      const owner = result.owner;
      const previousOwner = result.previousOwner;
      setMembers((items) => items.map((item) => {
        if (item.membershipId === owner.membershipId) return owner;
        if (item.membershipId === previousOwner.membershipId) return previousOwner;
        return item;
      }));
      setTransferTarget(null);
      setTransferPassword("");
      setNotice({ kind: "success", text: t("workspaceMembersRoute.ownershipTransferredToValue", { value1: owner.email }) });
    } catch (error) {
      await handleMutationError(error);
    } finally {
      setBusy(key, false);
    }
  }

  async function issueInvitation(email, role) {
    setBusy("invite", true);
    try {
      const result = await apiJson(
        `/api/workspaces/${encodeURIComponent(workspace.id)}/invitations`,
        jsonOptions("POST", { email: email.trim(), role }),
      );
      const invitation = result.invitation;
      if (typeof result.inviteUrl !== "string" || !result.inviteUrl) throw new Error("workspace_invite_url_invalid");
      setInvitations((items) => [...items.filter((item) => item.email !== invitation.email), invitation]);
      setInviteDialog({ email: invitation.email, role: invitation.role, inviteUrl: result.inviteUrl });
      setReissueTarget(null);
    } catch (error) {
      await handleMutationError(error);
    } finally {
      setBusy("invite", false);
    }
  }

  async function revokeInvitation() {
    if (!revokeTarget) return;
    const target = revokeTarget;
    const key = `revoke:${target.id}`;
    setBusy(key, true);
    try {
      const result = await apiJson(
        `/api/workspaces/${encodeURIComponent(workspace.id)}/invitations/${encodeURIComponent(target.id)}`,
        { method: "DELETE" },
      );
      if (result.ok !== true) throw new Error("workspace_invitation_revoke_result_invalid");
      setInvitations((items) => items.filter((item) => item.id !== target.id));
      setRevokeTarget(null);
      setNotice({ kind: "success", text: t("workspaceMembersRoute.theInvitationForValueWasRevoked", { value1: target.email }) });
    } catch (error) {
      await handleMutationError(error);
    } finally {
      setBusy(key, false);
    }
  }

  async function copyInviteUrl() {
    try {
      await navigator.clipboard.writeText(inviteDialog.inviteUrl);
      setNotice({ kind: "success", text: t("workspaceMembersRoute.invitationLinkCopied") });
    } catch (error) {
      setNotice({ kind: "error", text: t("workspaceMembersRoute.unableToCopyInvitationLinkValue", { value1: errorText(error) }) });
    }
  }

  if (!canManageWorkspace) {
    return <Navigate replace to={`${base}/app`} state={{ workspaceNotice: t("settingsRoute.youDoNotHaveAccessToWorkspaceSettings") }} />;
  }

  const actorRole = members?.find((member) => member.userId === String(user.id))?.role || workspace.role;
  const inviteBusy = busyKeys.has("invite");
  const transferBusy = transferTarget ? busyKeys.has(`transfer:${transferTarget.membershipId}`) : false;
  const removeBusy = removeTarget ? busyKeys.has(`remove:${removeTarget.membershipId}`) : false;
  const revokeBusy = revokeTarget ? busyKeys.has(`revoke:${revokeTarget.id}`) : false;

  const content = <>
    <div className={`shWorkspaceSettingsPage ${embedded ? "isEmbedded" : ""}`}>
      <header className="shWorkspaceSettingsHeader">
        <div>
          <span>{workspace.name}{" "}{t("workspaceGroupsRoute.workspaceSettings")}</span>
          <h1>{t("settingsRoute.membersAndPermissions")}</h1>
        </div>
        <button className="shPrimaryButton" type="button" disabled={loading || Boolean(pageError)} onClick={() => setInviteDialog({ email: "", role: "member", inviteUrl: "" })}>
          <UserPlus aria-hidden="true" />{t("workspaceMembersRoute.inviteMember")}</button>
      </header>

      <nav className="shWorkspaceSettingsTabs" aria-label={t("workspaceGroupsRoute.permissionSettings")}>
        <span className="isActive" aria-current="page">{t("workspaceGroupsRoute.membersAndInvitations")}</span>
        <Link to={`${base}/settings/groups`}>{t("settingsRoute.groups")}</Link>
      </nav>

      {loading ? <div className="shWorkspaceSettingsState" aria-live="polite">{t("workspaceMembersRoute.loadingMembersAndInvitations")}</div> : null}
      {!loading && pageError ? <div className="shWorkspaceSettingsState isError" role="alert">{t("workspaceMembersRoute.unableToLoadWorkspaceSettings")}{pageError}<button type="button" onClick={() => void load()}>{t("router.reloadPage")}</button></div> : null}

      {!loading && !pageError && members ? <>
        <section className="shWorkspaceSettingsSection">
          <header><div><h2>{t("invitationActivationRoute.member")}</h2><p>{t("members.count", { count: members.length })}</p></div></header>
          <div className="shWorkspaceMemberList">
            {members.map((member) => {
              const isCurrent = member.userId === String(user.id);
              const canMutate = member.role !== "owner" && !isCurrent;
              const memberBusy = busyKeys.has(`member:${member.membershipId}`);
              return <div className="shWorkspaceMemberRow" key={member.membershipId}>
                <span className="shWorkspaceMemberAvatar" aria-hidden="true">{member.email.slice(0, 1).toUpperCase()}</span>
                <span className="shWorkspaceMemberIdentity"><strong>{member.email}{isCurrent ? t("workspaceMembersRoute.you") : ""}</strong><small>{t("workspaceMembersRoute.joined")}{" "}{formatDate(member.createdAt)}</small></span>
                {canMutate ? <select value={member.role} disabled={memberBusy} aria-label={t("workspaceMembersRoute.roleForValue", { value1: member.email })} onChange={(event) => void updateRole(member, event.target.value)}>
                  <option value="admin">{t("invitationActivationRoute.administrator")}</option><option value="member">{t("invitationActivationRoute.member")}</option>
                </select> : <span className="shWorkspaceRoleLabel">{roleLabel(member.role)}</span>}
                {canMutate && !memberBusy ? <details className="shWorkspaceRowMenu">
                  <summary role="button" aria-label={t("workspaceMembersRoute.memberActionsForValue", { value1: member.email })}><MoreHorizontal aria-hidden="true" /></summary>
                  <div>
                    {actorRole === "owner" ? <button type="button" onClick={(event) => { event.currentTarget.closest("details").removeAttribute("open"); setTransferPassword(""); setTransferTarget(member); }}>{t("workspaceMembersRoute.transferOwnership")}</button> : null}
                    <button className="isDanger" type="button" onClick={(event) => { event.currentTarget.closest("details").removeAttribute("open"); setRemoveTarget(member); }}>{t("workspaceMembersRoute.removeMember")}</button>
                  </div>
                </details> : <span />}
              </div>;
            })}
          </div>
        </section>

        <section className="shWorkspaceSettingsSection">
          <header><div><h2>{t("workspaceMembersRoute.pendingInvitations")}</h2><p>{t("workspaceMembersRoute.invitationLinksAreShownOnlyWhenIssuedYouCan")}</p></div></header>
          {invitations.length ? <div className="shWorkspaceInvitationList">
            {invitations.map((invitation) => <div className="shWorkspaceInvitationRow" key={invitation.id}>
              <span className="shWorkspaceMemberIdentity"><strong>{invitation.email}</strong><small>{t("workspaceMembersRoute.created")}{" "}{formatDate(invitation.createdAt)}</small></span>
              <span>{roleLabel(invitation.role)}</span>
              <small>{formatDate(invitation.expiresAt)}{" "}{t("workspaceMembersRoute.expires")}</small>
              <span className="shWorkspaceInvitationActions">
                <button type="button" onClick={() => setReissueTarget(invitation)}>{t("workspaceMembersRoute.reissue")}</button>
                <button type="button" onClick={() => setRevokeTarget(invitation)}>{t("workspaceMembersRoute.revoke")}</button>
              </span>
            </div>)}
          </div> : <div className="shWorkspaceSettingsEmpty">{t("workspaceMembersRoute.noPendingInvitations")}</div>}
        </section>
      </> : null}
    </div>

    {notice ? <div className={`shWorkspaceToast ${notice.kind === "error" ? "isError" : ""}`} role={notice.kind === "error" ? "alert" : "status"}>{notice.text}</div> : null}

    {inviteDialog ? <WorkspaceDialog
      title={inviteDialog.inviteUrl ? t("workspaceMembersRoute.invitationLinkCreated") : t("workspaceMembersRoute.inviteMember")}
      description={inviteDialog.inviteUrl ? t("workspaceMembersRoute.thisLinkIsShownOnlyAfterThisIssuance") : t("workspaceMembersRoute.createAnInvitationLinkValidFor72HoursYou")}
      busy={inviteBusy}
      onClose={() => !inviteBusy && setInviteDialog(null)}
    >
      {inviteDialog.inviteUrl ? <div className="shWorkspaceInviteResult">
        <strong>{inviteDialog.email}</strong>
        <code>{inviteDialog.inviteUrl}</code>
        <p>{t("workspaceMembersRoute.previousUnacceptedInvitationsForThisEmailAreNowInvalid")}</p>
        <footer><button type="button" onClick={() => setInviteDialog(null)}>{t("workspaceMembersRoute.done")}</button><button className="isPrimary" type="button" onClick={() => void copyInviteUrl()}><Copy aria-hidden="true" />{t("workspaceMembersRoute.copyLink")}</button></footer>
      </div> : <form onSubmit={(event) => { event.preventDefault(); void issueInvitation(inviteDialog.email, inviteDialog.role); }}>
        <label>{t("loginForm.email")}<input autoFocus type="email" required value={inviteDialog.email} onChange={(event) => setInviteDialog({ ...inviteDialog, email: event.target.value })} /></label>
        <label>{t("invitationActivationRoute.role")}<select value={inviteDialog.role} onChange={(event) => setInviteDialog({ ...inviteDialog, role: event.target.value })}><option value="member">{t("invitationActivationRoute.member")}</option><option value="admin">{t("invitationActivationRoute.administrator")}</option></select></label>
        <footer><button type="button" disabled={inviteBusy} onClick={() => setInviteDialog(null)}>{t("agentRunRow.cancel")}</button><button className="isPrimary" type="submit" disabled={inviteBusy || !inviteDialog.email.trim()}>{inviteBusy ? t("workspaceMembersRoute.creating") : t("workspaceMembersRoute.createInvitationLink")}</button></footer>
      </form>}
    </WorkspaceDialog> : null}

    {transferTarget ? <WorkspaceDialog title={t("workspaceMembersRoute.transferWorkspaceOwnership")} description={t("workspaceMembersRoute.transferValueToValueYouWillBecomeAnAdministrator", { value1: workspace.name, value2: transferTarget.email })} busy={transferBusy} onClose={() => { setTransferTarget(null); setTransferPassword(""); }}>
      <form onSubmit={transferOwnership}>
        <label>{t("settingsRoute.currentPassword")}<input autoFocus type="password" autoComplete="current-password" required value={transferPassword} onChange={(event) => setTransferPassword(event.target.value)} /></label>
        <footer><button type="button" disabled={transferBusy} onClick={() => { setTransferTarget(null); setTransferPassword(""); }}>{t("agentRunRow.cancel")}</button><button className="isDanger" type="submit" disabled={transferBusy || !transferPassword}>{transferBusy ? t("workspaceMembersRoute.transferring") : t("workspaceMembersRoute.confirmTransfer")}</button></footer>
      </form>
    </WorkspaceDialog> : null}

    <ConfirmDialog open={Boolean(removeTarget)} title={t("workspaceMembersRoute.removeMember")} message={removeTarget ? t("workspaceMembersRoute.removeValueTheirMembershipAndCustomGroupMembershipsWill", { value1: removeTarget.email }) : ""} confirmLabel={t("globalPluginSettings.remove")} busy={removeBusy} onCancel={() => setRemoveTarget(null)} onConfirm={() => void removeMember()} />
    <ConfirmDialog open={Boolean(reissueTarget)} title={t("workspaceMembersRoute.reissueInvitation")} message={reissueTarget ? t("workspaceMembersRoute.issueANewLinkForValueTheOldLink", { value1: reissueTarget.email }) : ""} confirmLabel={t("workspaceMembersRoute.reissue")} busy={inviteBusy} onCancel={() => setReissueTarget(null)} onConfirm={() => void issueInvitation(reissueTarget.email, reissueTarget.role)} />
    <ConfirmDialog open={Boolean(revokeTarget)} title={t("workspaceMembersRoute.revokeInvitation")} message={revokeTarget ? t("workspaceMembersRoute.revokeTheInvitationForValueTheExistingLinkWill", { value1: revokeTarget.email }) : ""} confirmLabel={t("workspaceMembersRoute.revoke")} busy={revokeBusy} onCancel={() => setRevokeTarget(null)} onConfirm={() => void revokeInvitation()} />
  </>;
  return embedded ? content : <ShellPage>{content}</ShellPage>;
}
