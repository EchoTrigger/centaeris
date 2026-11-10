import { animate } from "@centaeris/ui/motion";
import { useTranslation } from "../i18n";
import { useLayoutEffect, useRef, useState } from "react";
import { Archive, ArrowLeft, Cpu, Settings2, ShieldCheck, SlidersHorizontal, Terminal, Users, UsersRound } from "lucide-react";
import { Link, Navigate, useLoaderData, useLocation, useNavigate, useOutletContext, useRouteLoaderData } from "react-router";
import { ApiError, apiJson, jsonOptions } from "../api";
import { LanguageSelector } from "../components/LanguageSelector";
import { ThemeSelector } from "../components/ThemeSelector";
import { ExternalIdentitySettings } from "../components/ExternalLogin";
import { WorkspaceDialog } from "../components/WorkspaceDialog";
import { useEnterStartsNewLine, writeEnterStartsNewLine } from "../preferences";
import ModelSettings from "./ModelSettings";
import WorkspaceGroupsRoute from "./WorkspaceGroupsRoute";
import WorkspaceMembersRoute from "./WorkspaceMembersRoute";
import EnvironmentSettings from "./EnvironmentSettings";
import WorkspaceProfileSettings from "./WorkspaceProfileSettings";
import { SessionArchive } from "./SessionArchive";
import { DEVELOPER_DOC_GROUPS, developerDocSearch, developerTopic, DeveloperSettings } from "./DeveloperSettings";

const ADMIN_ROLES = new Set(["owner", "admin"]);
const SECTIONS = (t) => ({
  preferences: { label: t("settingsRoute.preferences"), title: t("settingsRoute.preferences"), icon: SlidersHorizontal, group: t("settingsRoute.personal"), account: true },
  general: { label: t("pluginSettings.general"), title: t("pluginSettings.general"), icon: Settings2, group: t("settingsRoute.personal"), account: true },
  security: { label: t("settingsRoute.security"), title: t("settingsRoute.security"), icon: ShieldCheck, group: t("settingsRoute.personal"), account: true },
  "workspace-profile": { label: t("workspaceProfile.title"), title: t("workspaceProfile.title"), icon: Settings2, group: t("settingsRoute.workspace") },
  members: { label: t("invitationActivationRoute.member"), title: t("settingsRoute.membersAndPermissions"), icon: Users, group: t("settingsRoute.workspace") },
  environment: { label: t("environmentSettings.title"), title: t("environmentSettings.title"), icon: Settings2, group: t("settingsRoute.workspace") },
  archive: { label: t("archive.title"), title: t("archive.title"), icon: Archive, workspaceMember: true },
  groups: { label: t("settingsRoute.groups"), title: t("settingsRoute.groups"), icon: UsersRound, group: t("settingsRoute.workspace"), hiddenInNav: true },
  models: { label: t("appRoute.models"), title: t("appRoute.models"), icon: Cpu, group: t("settingsRoute.workspace"), resourceManagement: true },
  developer: { label: t("developer.title"), title: t("developer.title"), icon: Terminal, group: t("settingsRoute.workspace"), workspaceMember: true },
});

function Preferences({ userId }) {
  const { t } = useTranslation();
  const enterStartsNewLine = useEnterStartsNewLine(userId);

  function updateEnterBehavior(event) {
    const enabled = event.target.checked;
    writeEnterStartsNewLine(userId, enabled);
  }

  return <div className="preferenceSettings">
    <header className="settingsPageHeading"><h1>{t("settingsRoute.preferences")}</h1></header>
    <ThemeSelector />
    <div className="settingsPanel">
      <label className="preferenceRow">
        <span><strong>{t("settingsRoute.useEnterToStartANewLine")}</strong><small>{t("preferences.enterHint", { shortcut: "Cmd/Ctrl + Enter" })}</small></span>
        <input type="checkbox" role="switch" aria-label={t("settingsRoute.useEnterToStartANewLine")} checked={enterStartsNewLine} onChange={updateEnterBehavior} />
      </label>
    </div>
  </div>;
}

function GeneralSettings() {
  const { t } = useTranslation();
  return <div className="preferenceSettings">
    <header className="settingsPageHeading"><h1>{t("pluginSettings.general")}</h1></header>
    <div className="settingsPanel">
      <LanguageSelector />
    </div>
  </div>;
}

function AccountSecurity() {
  const { t } = useTranslation();
  const [editing, setEditing] = useState(false);
  const [currentPassword, setCurrentPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  function closeEditor() {
    setEditing(false);
    setCurrentPassword("");
    setNewPassword("");
    setConfirmation("");
    setError("");
  }

  function update(setter) {
    return (event) => {
      setter(event.target.value);
      setError("");
      setNotice("");
    };
  }

  async function submit(event) {
    event.preventDefault();
    if (busy || !currentPassword || !newPassword || !confirmation) return;
    if (newPassword !== confirmation) {
      setError(t("settingsRoute.theNewPasswordsDoNotMatch"));
      return;
    }
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await apiJson("/api/account/password", jsonOptions("PATCH", { currentPassword, newPassword }));
      closeEditor();
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

  return <div className="preferenceSettings">
    <header className="settingsPageHeading"><h1>{t("settingsRoute.security")}</h1></header>
    <div className="settingsPanel settingsListPanel">
      <div className="settingsEditRow">
        <span>{t("loginForm.password")}</span>
        <button className="secondary settingsEditButton" type="button" onClick={() => { setNotice(""); setEditing(true); }}>{t("passwordResetRoute.updatePassword")}</button>
      </div>
      {notice ? <p className="settingsEditNotice" role="status">{notice}</p> : null}
    </div>
    <ExternalIdentitySettings />
    {editing ? <WorkspaceDialog className="settingsEditDialog" title={t("passwordResetRoute.updatePassword")} busy={busy} onClose={closeEditor}>
      <form onSubmit={submit}>
        <label>{t("settingsRoute.currentPassword")}<input autoFocus autoComplete="current-password" type="password" value={currentPassword} onChange={update(setCurrentPassword)} required disabled={busy} /></label>
        <label>
          <span>{t("passwordResetRoute.newPassword")}</span>
          <input aria-label={t("passwordResetRoute.newPassword")} aria-describedby="new-password-hint" autoComplete="new-password" type="password" minLength={15} value={newPassword} onChange={update(setNewPassword)} required disabled={busy} />
          <small id="new-password-hint">{t("settingsRoute.atLeast15Characters")}</small>
        </label>
        <label>{t("settingsRoute.confirmNewPassword")}<input autoComplete="new-password" type="password" minLength={15} value={confirmation} onChange={update(setConfirmation)} required disabled={busy} /></label>
        {error ? <p className="settingsEditError" role="alert">{error}</p> : null}
        <footer>
          <button className="secondary" type="button" disabled={busy} onClick={closeEditor}>{t("agentRunRow.cancel")}</button>
          <button className="isPrimary" type="submit" disabled={busy || !currentPassword || !newPassword || !confirmation}>{busy ? t("passwordResetRoute.updating") : t("passwordResetRoute.updatePassword")}</button>
        </footer>
      </form>
    </WorkspaceDialog> : null}
  </div>;
}

function safeReturnTo(value, workspace) {
  if (typeof value !== "string" || !workspace) return null;
  return value.startsWith(`/w/${encodeURIComponent(workspace.id)}/`) ? value : null;
}

function focusSettingsContent(element) {
  element?.focus();
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
  const searchParams = new URLSearchParams(location.search);
  const docTopic = section === "developer" && searchParams.get("view") === "docs" ? developerTopic(searchParams.get("topic")) : null;
  const showingDocs = docTopic !== null;
  const navigationRef = useRef(null);
  const previouslyShowingDocs = useRef(showingDocs);
  useLayoutEffect(() => {
    const changed = previouslyShowingDocs.current !== showingDocs;
    previouslyShowingDocs.current = showingDocs;
    const nav = navigationRef.current;
    if (!changed || !nav || window.matchMedia?.("(prefers-reduced-motion: reduce)").matches) return;
    const playback = animate(nav, { opacity: [0.65, 1] }, { duration: 0.12, ease: "easeOut" });
    return () => playback.cancel();
  }, [showingDocs]);
  const workspace = workspaceData?.workspace || (isAccountRoute ? accountData?.workspace : null);
  const home = workspace ? `/w/${encodeURIComponent(workspace.id)}/app` : "/";
  const closeTo = safeReturnTo(location.state?.returnTo, workspace) || safeReturnTo(chat?.returnTo, workspace) || home;
  const lastDocTopic = docTopic || developerTopic(location.state?.developerTopic);
  const developerNavigation = { returnTo: closeTo, developerTopic: lastDocTopic };
  const canManageWorkspace = Boolean(workspace && ADMIN_ROLES.has(workspace.role));
  const canManageResources = Boolean(workspace && (canManageWorkspace || user?.isSuperuser));
  const sections = SECTIONS(t);
  const selected = sections[section];
  const allowedSections = Object.entries(sections).filter(([, item]) => (
    item.account || (item.workspaceMember ? Boolean(workspace) : item.resourceManagement ? canManageResources : canManageWorkspace)
  ));

  if (!selected) {
    return <Navigate replace to={isAccountRoute ? "/settings/preferences" : `${home.replace(/\/app$/, "")}/settings/general`} />;
  }
  if (!allowedSections.some(([key]) => key === section)) return <Navigate replace to={home} state={{ workspaceNotice: t("settingsRoute.youDoNotHaveAccessToWorkspaceSettings") }} />;

  const groups = [t("settingsRoute.personal"), t("settingsRoute.workspace"), undefined].map((label) => ({
    label,
    entries: allowedSections.filter(([, item]) => item.group === label && !item.hiddenInNav),
  })).filter(({ entries }) => entries.length);

  function sectionPath(key) {
    if (!workspaceData && sections[key].account) {
      return `/settings/${key}${workspace ? `?${new URLSearchParams({ workspaceId: workspace.id })}` : ""}`;
    }
    return `/w/${encodeURIComponent(workspace.id)}/settings/${key}`;
  }

  const body = section === "preferences" ? <Preferences userId={user.id} />
    : section === "security" ? <AccountSecurity />
    : section === "workspace-profile" ? <WorkspaceProfileSettings key={workspace.id} workspace={workspace} />
    : section === "members" ? <WorkspaceMembersRoute embedded />
    : section === "archive" ? <SessionArchive key={workspace.id} workspace={workspace} returnTo={closeTo} />
    : section === "developer" ? <DeveloperSettings key={workspace.id} workspaceId={workspace.id} canManage={canManageWorkspace}
      topic={docTopic} lastTopic={lastDocTopic} navigationState={developerNavigation} />
    : section === "environment" ? <EnvironmentSettings key={workspace.id} workspace={workspace} />
      : section === "groups" ? <WorkspaceGroupsRoute embedded />
        : section === "models" ? <ModelSettings key={workspace.id} workspace={workspace} onModelsChanged={() => chat?.onModelsChanged?.()} />
          : <GeneralSettings />;
  const content = section === "models" ? <div className="workspaceSettingsFeature">
    <header><h1>{selected.title}</h1></header>
    <div>{body}</div>
  </div> : body;
  return (
    <div className="workspaceSettingsPage">
      <aside className="workspaceSettingsNav">
        <Link className="workspaceSettingsBack" to={docTopic ? sectionPath("developer") : closeTo} state={docTopic ? developerNavigation : undefined}>
          <ArrowLeft aria-hidden="true" />{t(docTopic ? "developer.backToSettings" : "settingsRoute.backToApp")}
        </Link>
        {docTopic ? <>
          <nav ref={navigationRef} className="workspaceSettingsSections" aria-label={t("developer.docs")}>
            {DEVELOPER_DOC_GROUPS.map(({ label, topics }) => <section key={label}>
              <h2>{t(label)}</h2>
              {topics.map(topic => <Link key={topic} aria-current={topic === docTopic ? "page" : undefined}
                to={{ search: developerDocSearch(topic) }} state={developerNavigation}>{t(`developer.doc.${topic}.title`)}</Link>)}
            </section>)}
          </nav>
          <select className="workspaceSettingsSectionPicker" aria-label={t("developer.docs")} value={docTopic}
            onChange={event => navigate({ search: developerDocSearch(event.target.value) }, { state: developerNavigation })}>
            {DEVELOPER_DOC_GROUPS.map(({ label, topics }) => <optgroup key={label} label={t(label)}>
              {topics.map(topic => <option key={topic} value={topic}>{t(`developer.doc.${topic}.title`)}</option>)}
            </optgroup>)}
          </select>
        </> : <>
        <nav ref={navigationRef} className="workspaceSettingsSections" aria-label={t("shellSidebar.settings")}>
          {groups.map(({ label, entries }) => <section key={label ?? "archive"}>
            {label ? <h2>{label}</h2> : null}
            {entries.map(([key, item]) => {
              const Icon = item.icon;
              const active = key === section || (key === "members" && section === "groups");
              return <Link aria-current={active ? "page" : undefined} to={sectionPath(key)} state={{ returnTo: closeTo }} key={key}><Icon aria-hidden="true" />{item.label}</Link>;
            })}
          </section>)}
        </nav>
        <select className="workspaceSettingsSectionPicker" aria-label={t("shellSidebar.settings")} value={section === "groups" ? "members" : section}
          onChange={event => navigate(sectionPath(event.target.value), { state: { returnTo: closeTo } })}>
          {groups.map(({ label, entries }) => label ? <optgroup key={label} label={label}>
            {entries.map(([key, item]) => <option key={key} value={key}>{item.label}</option>)}
          </optgroup> : entries.map(([key, item]) => <option key={key} value={key}>{item.label}</option>))}
        </select>
        </>}
      </aside>
      <main className="workspaceSettingsContent" aria-label={docTopic ? t(`developer.doc.${docTopic}.title`) : selected.title}
        ref={focusSettingsContent} tabIndex={-1} key={docTopic || section}>
        <div className="workspaceSettingsScroll">{content}</div>
      </main>
    </div>
  );
}
