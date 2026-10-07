import { t } from "../i18n";
import { useTranslation } from "../i18n";
import { useState } from "react";
import { Bot, Boxes, Cpu, Plug, Settings2, ShieldCheck, SlidersHorizontal, Users, UsersRound, X } from "lucide-react";
import { Link, Navigate, useLoaderData, useLocation, useNavigate, useOutletContext, useRouteLoaderData } from "react-router";
import { ApiError, apiJson, jsonOptions } from "../api";
import { useModalDialog } from "../components/useModalDialog";
import { LanguageSelector } from "../components/LanguageSelector";
import { ThemeSelector } from "../components/ThemeSelector";
import { useEnterStartsNewLine, writeEnterStartsNewLine } from "../preferences";
import ModelSettings from "./ModelSettings";
import GlobalPluginSettings from "./GlobalPluginSettings";
import PluginSettings from "./PluginSettings";
import WorkspaceGroupsRoute from "./WorkspaceGroupsRoute";
import WorkspaceMembersRoute from "./WorkspaceMembersRoute";
import AppDelegations from "./AppDelegations";
import AgentSettings from "./AgentSettings";
import { canOpenPrivateAgentSettings } from "../agent-chat/agentSettingsNavigation";

const ADMIN_ROLES = new Set(["owner", "admin"]);
const SECTIONS = () => ({
  preferences: { label: t("settingsRoute.preferences"), title: t("settingsRoute.preferences"), icon: SlidersHorizontal, group: "", account: true },
  general: { label: t("pluginSettings.general"), title: t("pluginSettings.general"), icon: Settings2, group: "", account: true },
  applications: { label: t("appDelegations.title"), title: t("appDelegations.title"), icon: Boxes, group: "", account: true },
  agents: { label: t("agentSettings.agents"), title: t("agentSettings.agents"), icon: Bot, group: t("settingsRoute.workspace"), privateAgents: true },
  members: { label: t("invitationActivationRoute.member"), title: t("settingsRoute.membersAndPermissions"), icon: Users, group: t("settingsRoute.workspace") },
  groups: { label: t("settingsRoute.groups"), title: t("settingsRoute.groups"), icon: UsersRound, group: t("settingsRoute.workspace"), hiddenInNav: true },
  plugins: { label: t("libraryRoute.plugins"), title: t("settingsRoute.workspacePlugins"), icon: Plug, group: t("settingsRoute.workspace") },
  security: { label: t("settingsRoute.security"), title: t("settingsRoute.security"), icon: ShieldCheck, group: t("invitationActivationRoute.administrator"), account: true },
  models: { label: t("appRoute.models"), title: t("appRoute.models"), icon: Cpu, group: t("invitationActivationRoute.administrator"), superuser: true },
  "global-plugins": { label: t("settingsRoute.platformPlugins"), title: t("settingsRoute.platformPlugins"), icon: Boxes, group: t("invitationActivationRoute.administrator"), superuser: true },
});

function Preferences({ userId }) {
  const { t } = useTranslation();
  const enterStartsNewLine = useEnterStartsNewLine(userId);

  function updateEnterBehavior(event) {
    const enabled = event.target.checked;
    writeEnterStartsNewLine(userId, enabled);
  }

  return <div className="preferenceSettings">
    <section aria-labelledby="input-preferences-heading">
      <h1 id="input-preferences-heading">{t("settingsRoute.inputOptions")}</h1>
      <label className="preferenceRow">
        <span><strong>{t("settingsRoute.useEnterToStartANewLine")}</strong><small>{t("preferences.enterHint", { shortcut: "Cmd/Ctrl + Enter" })}</small></span>
        <input type="checkbox" role="switch" aria-label={t("settingsRoute.useEnterToStartANewLine")} checked={enterStartsNewLine} onChange={updateEnterBehavior} />
      </label>
    </section>
  </div>;
}

function GeneralSettings() {
  const { t } = useTranslation();
  return <div className="preferenceSettings">
    <section>
      <h1>{t("pluginSettings.general")}</h1>
      <LanguageSelector />
      <ThemeSelector />
    </section>
  </div>;
}

function AccountSecurity() {
  const { t } = useTranslation();
  const [currentPassword, setCurrentPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  function update(setter) {
    return (event) => {
      setter(event.target.value);
      setError("");
      setNotice("");
    };
  }

  async function submit(event) {
    event.preventDefault();
    if (newPassword !== confirmation) {
      setError(t("settingsRoute.theNewPasswordsDoNotMatch"));
      return;
    }
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await apiJson("/api/account/password", jsonOptions("PATCH", { currentPassword, newPassword }));
      setCurrentPassword("");
      setNewPassword("");
      setConfirmation("");
      setNotice(t("settingsRoute.passwordUpdatedThisDeviceStaysSignedInOtherSessions"));
    } catch (requestError) {
      const messages = {
        account_current_password_invalid: t("settingsRoute.theCurrentPasswordIsIncorrect"),
        account_password_invalid: t("settingsRoute.theNewPasswordDoesNotMeetTheCurrentSecurity"),
        account_password_unchanged: t("settingsRoute.theNewPasswordMustDifferFromTheCurrentPassword"),
      };
      setError(requestError instanceof ApiError ? messages[requestError.message] || t("settingsRoute.unableToUpdateYourPasswordPleaseTryAgain") : t("settingsRoute.unableToUpdateYourPasswordPleaseTryAgain"));
    } finally {
      setBusy(false);
    }
  }

  return <div className="accountSecuritySettings">
    <header>
      <h1>{t("settingsRoute.security")}</h1>
    </header>
    <form className="accountSecurityForm" onSubmit={submit}>
      <label>{t("settingsRoute.currentPassword")}<input autoComplete="current-password" type="password" value={currentPassword} onChange={update(setCurrentPassword)} required /></label>
      <label>{t("passwordResetRoute.newPassword")}<input autoComplete="new-password" type="password" minLength={15} value={newPassword} onChange={update(setNewPassword)} required /><small>{t("settingsRoute.atLeast15Characters")}</small></label>
      <label>{t("settingsRoute.confirmNewPassword")}<input autoComplete="new-password" type="password" minLength={15} value={confirmation} onChange={update(setConfirmation)} required /></label>
      {error ? <p className="accountSecurityError" role="alert">{error}</p> : null}
      {notice ? <p className="accountSecurityNotice" role="status">{notice}</p> : null}
      <div className="accountSecurityActions"><button type="submit" disabled={busy || !currentPassword || !newPassword || !confirmation}>{busy ? t("passwordResetRoute.updating") : t("passwordResetRoute.updatePassword")}</button></div>
    </form>
  </div>;
}

function safeReturnTo(value, workspace) {
  if (typeof value !== "string" || !workspace) return null;
  return value.startsWith(`/w/${encodeURIComponent(workspace.id)}/`) ? value : null;
}

export default function SettingsPage() {
  const { t } = useTranslation();
  const { user } = useRouteLoaderData("authenticated");
  const workspaceData = useRouteLoaderData("workspace");
  const accountData = useLoaderData();
  const navigate = useNavigate();
  const location = useLocation();
  const chat = useOutletContext();
  const isAccountRoute = location.pathname.startsWith("/settings/");
  const section = location.pathname.split("/").at(-1);
  const workspace = workspaceData?.workspace || (isAccountRoute ? accountData?.workspace : null);
  const home = workspace ? `/w/${encodeURIComponent(workspace.id)}/app` : "/";
  const closeTo = safeReturnTo(chat?.returnTo, workspace) || safeReturnTo(location.state?.returnTo, workspace) || home;
  const canManageWorkspace = Boolean(workspace && ADMIN_ROLES.has(workspace.role));
  const canManagePrivateAgents = Boolean(workspaceData && canOpenPrivateAgentSettings(workspace));
  const [agentEditorOpen, setAgentEditorOpen] = useState(false);
  const visibleSections = Object.entries(SECTIONS()).filter(([, item]) => (
    item.privateAgents ? canManagePrivateAgents : item.account || (canManageWorkspace && (!item.superuser || user?.isSuperuser))
  ));
  const dialogRef = useModalDialog({ open: !agentEditorOpen, preserveOpener: section === "agents", onClose: () => navigate(closeTo) });

  if (SECTIONS()[section]?.privateAgents ? !canManagePrivateAgents : !SECTIONS()[section]?.account && !canManageWorkspace) return <Navigate replace to={home} state={{ workspaceNotice: t("settingsRoute.youDoNotHaveAccessToWorkspaceSettings") }} />;
  if (!SECTIONS()[section] || (SECTIONS()[section].superuser && !user?.isSuperuser)) {
    return <Navigate replace to={isAccountRoute ? "/settings/preferences" : `${home.replace(/\/app$/, "")}/settings/general`} />;
  }

  const body = section === "preferences" ? <Preferences userId={user.id} />
    : section === "security" ? <AccountSecurity />
    : section === "applications" ? <AppDelegations isSuperuser={Boolean(user?.isSuperuser)} />
    : section === "agents" ? <AgentSettings key={`${workspace.id}:${location.search}`} onEditorOpenChange={setAgentEditorOpen} />
    : section === "members" ? <WorkspaceMembersRoute embedded />
      : section === "groups" ? <WorkspaceGroupsRoute embedded />
        : section === "plugins" ? <PluginSettings workspace={workspace} isSuperuser={Boolean(user?.isSuperuser)} />
          : section === "global-plugins" ? <GlobalPluginSettings />
          : section === "models" ? <ModelSettings onClose={() => navigate(closeTo)} onModelsChanged={() => chat?.onModelsChanged()} />
            : <GeneralSettings />;
  const content = ["plugins", "models", "global-plugins"].includes(section) ? <div className={`workspaceSettingsFeature ${section === "models" ? "isModels" : "isCapabilities"}`}>
    <header><h1>{SECTIONS()[section].title}</h1></header>
    <div>{body}</div>
  </div> : body;
  return (
    <main className="settingsModalPage" onMouseDown={() => { if (!agentEditorOpen) navigate(closeTo); }}>
      <section className={`settingsDialog workspaceSettingsDialog${section === "agents" ? " isAgentSettings" : section === "applications" ? " isApplicationSettings" : ""}`} ref={dialogRef} role="dialog" aria-modal="true" aria-label={SECTIONS()[section].title} tabIndex={-1} onMouseDown={(event) => event.stopPropagation()}>
        <button className="quietCloseButton workspaceSettingsClose" type="button" onClick={() => navigate(closeTo)} aria-label={t("workspaceContextPanel.close")}><X aria-hidden="true" /></button>
        <aside className="workspaceSettingsNav">
          {["", t("settingsRoute.workspace"), t("invitationActivationRoute.administrator")].map((group) => {
            const entries = visibleSections.filter(([, item]) => item.group === group && !item.hiddenInNav);
            return entries.length ? <section key={group || "preferences"}>{group ? <h2>{group}</h2> : null}<nav>{entries.map(([key, item]) => {
              const Icon = item.icon;
              const target = item.account && !workspaceData
                ? `/settings/${key}${workspace ? `?${new URLSearchParams({ workspaceId: workspace.id })}` : ""}`
                : `/w/${encodeURIComponent(workspace.id)}/settings/${key}`;
              const active = key === section || (key === "members" && section === "groups");
              return <Link className={active ? "isActive" : ""} aria-current={active ? "page" : undefined} to={target} state={{ returnTo: closeTo }} key={key}><Icon aria-hidden="true" />{item.label}</Link>;
            })}</nav></section> : null;
          })}
        </aside>
        <div className="workspaceSettingsContent">
          <div className="workspaceSettingsScroll">{content}</div>
        </div>
      </section>
    </main>
  );
}
