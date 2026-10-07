import { useEffect, useRef, useState } from "react";
import { ApiError, apiJson, jsonOptions, requireWorkspaces } from "../api";
import { useTranslation } from "../i18n";
import { agentSettingsPath } from "../agent-chat/agentSettingsNavigation";
import AppDelegationDetails from "./AppDelegationDetails";
import "./app-delegations.css";

const SCOPES = ["assistant:use", "sessions:read", "sessions:create", "messages:submit", "attachments:write", "events:read", "artifacts:read", "runs:cancel"];
const NATIVE_SCOPES = new Set(["assistant:use", "sessions:read", "messages:submit", "artifacts:read"]);

function activeDelegation(delegation) {
  return !delegation.revokedAt && (delegation.expiresAt === null || Date.parse(delegation.expiresAt) > Date.now());
}

function requireIssuedDelegation(result) {
  if (!result.delegation?.id || !Number.isInteger(result.delegation.credentialVersion) || result.delegation.credentialVersion < 1 || typeof result.accessToken !== "string" || !result.accessToken || result.tokenType !== "Bearer") throw new Error("app_delegation_invalid");
  return result;
}

function requestErrorText(error, t) {
  const messages = {
    delegation_request_invalid: "appDelegations.invalidRequest",
    delegation_not_available: "appDelegations.notAvailable",
    delegation_credential_conflict: "appDelegations.credentialConflict",
    business_app_request_invalid: "appDelegations.invalidApp",
    business_app_revoked: "appDelegations.appRevoked",
  };
  return t(error instanceof ApiError && messages[error.message] ? messages[error.message] : "appDelegations.requestFailed");
}

function requireApps(value) {
  if (!Array.isArray(value) || value.some(app => typeof app?.id !== "string" || !app.id || typeof app.name !== "string" || !["pending", "active", "revoked"].includes(app.status))) throw new Error("business_apps_invalid");
  return value;
}

function requireBusinessBranchPage(result, rootAgentId) {
  if (Object.keys(result).sort().join("|") !== "branches|nextAfterId" || !(result.nextAfterId === null || (typeof result.nextAfterId === "string" && result.nextAfterId))
    || !Array.isArray(result.branches) || result.branches.some(branch =>
      branch.rootAgentId !== rootAgentId || !["branchId", "businessUserId", "agentId", "sessionId", "createdAt"].every(key => typeof branch[key] === "string" && branch[key])
      || !["active", "deleted"].includes(branch.status) || !Number.isInteger(branch.sessionCount) || branch.sessionCount < 0)) throw new Error("business_branches_invalid");
  return result;
}

function requireBranchSessionPage(result, branch) {
  const validSession = session => session && ["sessionId", "createdAt"].every(key => typeof session[key] === "string" && session[key]) && typeof session.title === "string" && ["active", "deleted"].includes(session.status);
  if (Object.keys(result).sort().join("|") !== "branchId|coordinationSession|nextAfterSessionId|workSessions" || result.branchId !== branch.branchId
    || !validSession(result.coordinationSession) || result.coordinationSession.sessionId !== branch.sessionId
    || !(result.nextAfterSessionId === null || (typeof result.nextAfterSessionId === "string" && result.nextAfterSessionId))
    || !Array.isArray(result.workSessions) || result.workSessions.some(item => !validSession(item) || typeof item.sourceAgentRunId !== "string" || !item.sourceAgentRunId)) throw new Error("business_branch_sessions_invalid");
  return result;
}

function BranchSessions({ rootAgentId, appId, branch }) {
  const { t, i18n } = useTranslation();
  const [page, setPage] = useState(null);
  const [error, setError] = useState("");
  const [loadingMore, setLoadingMore] = useState(false);
  const [moreError, setMoreError] = useState("");
  const [retry, setRetry] = useState(0);
  const requestController = useRef(null);
  const branchId = branch.branchId;
  const sessionId = branch.sessionId;
  // biome-ignore lint/correctness/useExhaustiveDependencies: retry explicitly reloads a failed request for the same branch.
  useEffect(() => {
    const controller = new AbortController(); requestController.current = controller;
    setPage(null); setError(""); setMoreError(""); setLoadingMore(false);
    const query = new URLSearchParams({ appId });
    apiJson(`/api/agents/${encodeURIComponent(rootAgentId)}/business-branches/${encodeURIComponent(branchId)}/sessions?${query}`, { signal: controller.signal })
      .then(result => { const next = requireBranchSessionPage(result, { branchId, sessionId }); if (!controller.signal.aborted) setPage(next); })
      .catch(() => { if (!controller.signal.aborted) setError(t("appDelegations.workSessionsFailed")); });
    return () => controller.abort();
  }, [rootAgentId, appId, branchId, sessionId, retry, t]);

  async function loadMore() {
    const controller = requestController.current;
    if (!page || page.nextAfterSessionId === null || loadingMore || !controller || controller.signal.aborted) return;
    setLoadingMore(true); setMoreError("");
    try {
      const query = new URLSearchParams({ appId, afterSessionId: page.nextAfterSessionId });
      const next = requireBranchSessionPage(await apiJson(`/api/agents/${encodeURIComponent(rootAgentId)}/business-branches/${encodeURIComponent(branchId)}/sessions?${query}`, { signal: controller.signal }), { branchId, sessionId });
      if (!controller.signal.aborted) setPage(current => ({ ...next, workSessions: [...new Map([...current.workSessions, ...next.workSessions].map(item => [item.sessionId, item])).values()] }));
    } catch { if (!controller.signal.aborted) setMoreError(t("appDelegations.workSessionsMoreFailed")); }
    finally { if (!controller.signal.aborted) setLoadingMore(false); }
  }

  function sessionNode(item, work = false) {
    return <li key={item.sessionId} className={work ? "appDelegationWorkSession" : "appDelegationCoordinationSession"}><strong>{item.title}</strong><small>{t(work ? "appDelegations.workSession" : "appDelegations.coordinationSession")} / {t(item.status === "deleted" ? "appDelegations.branchDeleted" : "appDelegations.status.active")}</small><code translate="no">{item.sessionId}</code>{work ? <small>{t("appDelegations.sourceRun")}: <code translate="no">{item.sourceAgentRunId}</code></small> : null}<small>{t("appDelegations.created", { date: new Date(item.createdAt).toLocaleString(i18n.language) })}</small></li>;
  }

  return <div className="appDelegationSessionTree" aria-busy={page === null && !error}>
    {error ? <><p role="alert">{error}</p><button type="button" onClick={() => setRetry(value => value + 1)}>{t("appDelegations.retryWorkSessions")}</button></> : page === null ? <p role="status">{t("appDelegations.workSessionsLoading")}</p> : <><ul>{sessionNode(page.coordinationSession)}{page.workSessions.map(item => sessionNode(item, true))}</ul>{!page.workSessions.length ? <p>{t("appDelegations.workSessionsEmpty")}</p> : null}</>}
    {loadingMore ? <p role="status">{t("appDelegations.workSessionsMoreLoading")}</p> : null}{moreError ? <p role="alert">{moreError}</p> : null}
    {page?.nextAfterSessionId ? <button type="button" disabled={loadingMore} onClick={() => void loadMore()}>{t(moreError ? "appDelegations.retryMoreWorkSessions" : "appDelegations.loadMoreWorkSessions")}</button> : null}
  </div>;
}

function BusinessBranch({ branch, delegation }) {
  const { t, i18n } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  return <li className="appDelegationBranch">
    <button type="button" className="appDelegationBranchToggle" aria-expanded={expanded} aria-controls={`branch-sessions-${branch.branchId}`} onClick={() => setExpanded(value => !value)}><span aria-hidden="true">{expanded ? "▾" : "▸"}</span><strong>{t("appDelegations.businessUserId")}: <code translate="no">{branch.businessUserId}</code></strong></button>
    <small>{t(branch.status === "deleted" ? "appDelegations.branchDeleted" : "appDelegations.status.active")}</small>
    <dl><dt>{t("appDelegations.branchId")}</dt><dd><code translate="no">{branch.branchId}</code></dd><dt>{t("appDelegations.branchAgentId")}</dt><dd><code translate="no">{branch.agentId}</code></dd><dt>{t("appDelegations.branchSessionId")}</dt><dd><code translate="no">{branch.sessionId}</code></dd><dt>{t("appDelegations.branchSessionCount")}</dt><dd>{branch.sessionCount}</dd></dl>
    <small>{t("appDelegations.created", { date: new Date(branch.createdAt).toLocaleString(i18n.language) })}</small>
    <div id={`branch-sessions-${branch.branchId}`}>{expanded ? <BranchSessions rootAgentId={delegation.agentId} appId={delegation.appId} branch={branch} /> : null}</div>
  </li>;
}

function BusinessBranches({ delegation, canConfigure, onClose }) {
  const { t } = useTranslation();
  const [branches, setBranches] = useState(null);
  const [error, setError] = useState("");
  const [nextAfterId, setNextAfterId] = useState(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const [moreError, setMoreError] = useState("");
  const requestController = useRef(null);
  const rootAgentId = delegation.agentId;
  const appId = delegation.appId;
  useEffect(() => {
    const controller = new AbortController();
    requestController.current = controller;
    setBranches(null); setError(""); setNextAfterId(null); setLoadingMore(false); setMoreError("");
    const query = new URLSearchParams({ appId });
    apiJson(`/api/agents/${encodeURIComponent(rootAgentId)}/business-branches?${query}`, { signal: controller.signal }).then(result => {
      const page = requireBusinessBranchPage(result, rootAgentId);
      if (!controller.signal.aborted) { setBranches(page.branches); setNextAfterId(page.nextAfterId); }
    }).catch(() => { if (!controller.signal.aborted) setError(t("appDelegations.branchesFailed")); });
    return () => { controller.abort(); };
  }, [rootAgentId, appId, t]);

  async function loadMore() {
    const controller = requestController.current;
    if (!controller || controller.signal.aborted || loadingMore || branches === null || nextAfterId === null) return;
    setLoadingMore(true); setMoreError("");
    try {
      const query = new URLSearchParams({ appId, afterBranchId: nextAfterId });
      const page = requireBusinessBranchPage(await apiJson(`/api/agents/${encodeURIComponent(rootAgentId)}/business-branches?${query}`, { signal: controller.signal }), rootAgentId);
      if (!controller.signal.aborted) {
        setBranches(rows => [...new Map([...rows, ...page.branches].map(branch => [branch.branchId, branch])).values()]);
        setNextAfterId(page.nextAfterId);
      }
    } catch { if (!controller.signal.aborted) setMoreError(t("appDelegations.branchesMoreFailed")); }
    finally { if (!controller.signal.aborted) setLoadingMore(false); }
  }

  return <section className="appDelegationSection appDelegationBranchView" aria-labelledby="business-branches-heading" aria-busy={branches === null && !error}>
    <h2 id="business-branches-heading">{t("appDelegations.branchesTitle")}</h2>
    <p>{delegation.appName} / {t("appDelegations.nativeAgentName", { name: delegation.agentName })}</p>
    <code translate="no">{rootAgentId}</code>
    <p>{t("appDelegations.branchesHint")}</p>
    <div className="appDelegationActions">
      <button type="button" onClick={onClose}>{t("appDelegations.closeBranches")}</button>
      {branches !== null && !error && canConfigure ? <a href={agentSettingsPath(delegation.workspaceId, rootAgentId)}>{t("appDelegations.rootConfiguration")}</a> : null}
    </div>
    {error ? <p role="alert" className="accountSecurityError">{error}</p> : branches === null ? <p role="status">{t("appDelegations.branchesLoading")}</p> : !branches.length ? <p>{t("appDelegations.branchesEmpty")}</p> : <ul className="appDelegationBranches">
      {branches.map(branch => <BusinessBranch key={branch.branchId} branch={branch} delegation={delegation} />)}
    </ul>}
    {loadingMore ? <p role="status">{t("appDelegations.branchesMoreLoading")}</p> : null}
    {moreError ? <p role="alert" className="accountSecurityError">{moreError}</p> : null}
    {nextAfterId !== null ? <button type="button" disabled={loadingMore} onClick={() => void loadMore()}>{t(moreError ? "appDelegations.branchesMoreRetry" : "appDelegations.branchesLoadMore")}</button> : null}
  </section>;
}

function BusinessAppAdmin({ onAppsChanged }) {
  const { t } = useTranslation();
  const [apps, setApps] = useState(null);
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    let active = true;
    apiJson("/api/admin/business-apps").then(result => {
      const rows = requireApps(result.apps);
      if (active) setApps(rows);
    }).catch(error => { if (active) setError(requestErrorText(error, t)); });
    return () => { active = false; };
  }, [t]);

  async function register(event) {
    event.preventDefault();
    if (busy || !name.trim() || !apps) return;
    setBusy(true); setError("");
    try {
      const result = await apiJson("/api/admin/business-apps", jsonOptions("POST", { name: name.trim() }));
      requireApps([result.app]);
      setApps(items => [...items, result.app]); setName("");
    } catch (error) { setError(requestErrorText(error, t)); }
    finally { setBusy(false); }
  }

  async function update(app, status) {
    if (busy || app.status === "revoked") return;
    setBusy(true); setError("");
    try {
      const result = await apiJson(`/api/admin/business-apps/${encodeURIComponent(app.id)}`, jsonOptions("PATCH", { status }));
      requireApps([result.app]);
      setApps(items => items.map(item => item.id === app.id ? result.app : item));
      await onAppsChanged();
    } catch (error) { setError(requestErrorText(error, t)); }
    finally { setBusy(false); }
  }

  return <section className="appDelegationSection" aria-labelledby="business-app-admin-heading">
    <h2 id="business-app-admin-heading">{t("appDelegations.manageApps")}</h2>
    <p>{t("appDelegations.adminHint")}</p>
    <form className="appDelegationForm" onSubmit={register}>
      <label>{t("appDelegations.appName")}<input aria-label={t("appDelegations.appName")} value={name} onChange={event => setName(event.target.value)} required disabled={busy} /></label>
      <button type="submit" disabled={busy || !apps || !name.trim()}>{t("appDelegations.registerApp")}</button>
    </form>
    {error ? <p role="alert" className="accountSecurityError">{error}</p> : null}
    {apps === null && !error ? <p role="status">{t("appDelegations.loading")}</p> : null}
    {apps?.map(app => <article className="appDelegationRow" key={app.id}>
      <div><strong>{app.name}</strong><small>{t(`appDelegations.status.${app.status}`)}</small></div>
      <div className="appDelegationActions">
        {app.status === "pending" ? <button type="button" data-action="activate" disabled={busy} onClick={() => void update(app, "active")}>{t("appDelegations.activate")}</button> : null}
        {app.status !== "revoked" ? <button type="button" data-action="revoke" disabled={busy} onClick={() => void update(app, "revoked")}>{t("appDelegations.revokeApp")}</button> : null}
      </div>
    </article>)}
  </section>;
}

export default function AppDelegations({ isSuperuser = false }) {
  const { t, i18n } = useTranslation();
  const [apps, setApps] = useState([]);
  const [workspaces, setWorkspaces] = useState([]);
  const [delegations, setDelegations] = useState(null);
  const [definitions, setDefinitions] = useState([]);
  const [agents, setAgents] = useState([]);
  const [loadedWorkspaceId, setLoadedWorkspaceId] = useState("");
  const [appId, setAppId] = useState("");
  const [workspaceId, setWorkspaceId] = useState("");
  const [targetId, setTargetId] = useState("");
  const [scopes, setScopes] = useState([]);
  const [expiry, setExpiry] = useState("3600");
  const [duration, setDuration] = useState("permanent");
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [oneTimeToken, setOneTimeToken] = useState(null);
  const [copied, setCopied] = useState(false);
  const [selectedDelegationId, setSelectedDelegationId] = useState(null);
  const [detailTab, setDetailTab] = useState("overview");
  const [formOpen, setFormOpen] = useState(false);

  useEffect(() => {
    let active = true;
    Promise.all([apiJson("/api/business-apps"), apiJson("/api/workspaces")]).then(([appResult, workspaceResult]) => {
      const rows = requireApps(appResult.apps);
      if (rows.some(app => app.status !== "active")) throw new Error("business_apps_invalid");
      const workspaceRows = requireWorkspaces(workspaceResult.workspaces);
      if (active) { setApps(rows); setWorkspaces(workspaceRows); }
    }).catch(error => { if (active) setError(requestErrorText(error, t)); });
    apiJson("/api/account/app-delegations").then(result => {
      if (!Array.isArray(result.delegations)) throw new Error("app_delegations_invalid");
      if (active) setDelegations(result.delegations);
    }).catch(error => { if (active) setError(requestErrorText(error, t)); });
    return () => { active = false; };
  }, [t]);

  useEffect(() => {
    if (!workspaceId) return undefined;
    let active = true;
    const workspacePath = `/api/workspaces/${encodeURIComponent(workspaceId)}`;
    Promise.all([apiJson(`${workspacePath}/available-agent-definitions`), apiJson(`${workspacePath}/agents`)]).then(([definitionResult, agentResult]) => {
      if (!Array.isArray(definitionResult.definitions) || definitionResult.definitions.some(item => !item.definitionId || typeof item.name !== "string")) throw new Error("agent_definitions_invalid");
      if (!Array.isArray(agentResult.agents) || agentResult.agents.some(item => typeof item.id !== "string" || !item.id || typeof item.name !== "string" || !(item.definitionId === null || typeof item.definitionId === "string"))) throw new Error("agents_invalid");
      if (active) { setDefinitions(definitionResult.definitions); setAgents(agentResult.agents.filter(item => item.definitionId === null)); setLoadedWorkspaceId(workspaceId); }
    }).catch(error => { if (active) setError(requestErrorText(error, t)); });
    return () => { active = false; };
  }, [workspaceId, t]);

  const app = apps.find(item => item.id === appId);
  const workspace = workspaces.find(item => item.id === workspaceId);
  const definition = loadedWorkspaceId === workspaceId ? definitions.find(item => `definition:${item.definitionId}` === targetId) : null;
  const agent = loadedWorkspaceId === workspaceId ? agents.find(item => `agent:${item.id}` === targetId) : null;
  const targetName = agent ? t("appDelegations.nativeAgentName", { name: agent.name }) : definition ? t("appDelegations.publishedAssistantName", { name: definition.name }) : "";
  const scopeChoices = agent ? SCOPES.filter(scope => NATIVE_SCOPES.has(scope)) : SCOPES;
  const expiresInSeconds = duration === "permanent" ? null : Number(expiry);
  const validExpiry = expiresInSeconds === null || (Number.isInteger(expiresInSeconds) && expiresInSeconds >= 300 && expiresInSeconds <= 86400);
  const canAuthorize = Boolean(app && workspace && (agent || definition) && scopes.length && scopes.every(scope => scopeChoices.includes(scope)) && validExpiry && !busy);
  const tokenDelegation = delegations?.find(item => item.id === oneTimeToken?.delegationId);
  const selectedDelegation = delegations?.find(item => item.id === selectedDelegationId);
  const groups = [...new Map((delegations || []).map(item => [item.appId, item.appName])).entries()];

  async function authorize(event) {
    event.preventDefault();
    if (!canAuthorize) return;
    setBusy("authorize"); setError(""); setOneTimeToken(null); setCopied(false);
    try {
      const target = agent ? { agentId: agent.id } : { definitionId: definition.definitionId };
      const result = requireIssuedDelegation(await apiJson("/api/account/app-delegations", jsonOptions("POST", { appId, workspaceId, ...target, scopes, expiresInSeconds })));
      setDelegations(items => [...(items || []), result.delegation]);
      setOneTimeToken({ delegationId: result.delegation.id, accessToken: result.accessToken });
    } catch (error) { setError(requestErrorText(error, t)); }
    finally { setBusy(""); }
  }

  async function rotate(delegation) {
    if (busy || !activeDelegation(delegation)) return;
    setBusy(delegation.id); setError(""); setOneTimeToken(null); setCopied(false);
    try {
      const result = requireIssuedDelegation(await apiJson(`/api/account/app-delegations/${encodeURIComponent(delegation.id)}/rotate`, jsonOptions("POST", { expectedCredentialVersion: delegation.credentialVersion })));
      setDelegations(items => items.map(item => item.id === delegation.id ? result.delegation : item));
      setOneTimeToken({ delegationId: result.delegation.id, accessToken: result.accessToken, rotated: true });
    } catch (error) {
      let message = requestErrorText(error, t);
      if (error instanceof ApiError && error.message === "delegation_credential_conflict") {
        try {
          const result = await apiJson("/api/account/app-delegations");
          if (!Array.isArray(result.delegations)) throw new Error("app_delegations_invalid");
          setDelegations(result.delegations);
        } catch { message = t("appDelegations.credentialRefreshFailed"); }
      }
      setError(message);
    }
    finally { setBusy(""); }
  }

  async function revoke(delegation) {
    if (busy || delegation.revokedAt) return;
    setBusy(delegation.id); setError("");
    try {
      await apiJson(`/api/account/app-delegations/${encodeURIComponent(delegation.id)}`, { method: "DELETE" });
      setDelegations(items => items.map(item => item.id === delegation.id ? { ...item, revokedAt: new Date().toISOString() } : item));
      if (oneTimeToken?.delegationId === delegation.id) setOneTimeToken(null);
    } catch (error) { setError(requestErrorText(error, t)); }
    finally { setBusy(""); }
  }

  async function copyToken() {
    try { await navigator.clipboard.writeText(oneTimeToken.accessToken); setCopied(true); }
    catch { setError(t("appDelegations.copyFailed")); }
  }

  async function reloadApps() {
    const result = await apiJson("/api/business-apps");
    const rows = requireApps(result.apps);
    if (rows.some(app => app.status !== "active")) throw new Error("business_apps_invalid");
    setApps(rows);
  }

  function dateText(value) { return new Date(value).toLocaleString(i18n.language); }

  return <div className="accountSecuritySettings appDelegations">
    <header className="appDelegationPageHeader"><h1>{t("appDelegations.title")}</h1>{!selectedDelegation ? <button type="button" data-action="new-authorization" aria-expanded={formOpen} aria-controls="app-delegation-consent" onClick={() => setFormOpen(value => !value)}>{t(formOpen ? "appDelegations.closeAuthorization" : "appDelegations.newAuthorization")}</button> : null}</header>
    {!selectedDelegation ? <p>{t("appDelegations.hint")}</p> : null}
    {error ? <p className="accountSecurityError" role="alert">{error}</p> : null}
    <form id="app-delegation-consent" className="appDelegationForm appDelegationSection appDelegationConsent" hidden={!formOpen || Boolean(selectedDelegation)} onSubmit={authorize}>
      <h2>{t("appDelegations.newAuthorization")}</h2>
      <label>{t("appDelegations.application")}<select aria-label={t("appDelegations.application")} value={appId} onChange={event => setAppId(event.target.value)} disabled={Boolean(busy)} required>
        <option value="">{t("appDelegations.chooseApp")}</option>{apps.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}
      </select></label>
      <label>{t("appDelegations.workspace")}<select aria-label={t("appDelegations.workspace")} value={workspaceId} disabled={Boolean(busy)} required onChange={event => {
        setWorkspaceId(event.target.value); setTargetId(""); setDefinitions([]); setAgents([]); setLoadedWorkspaceId(""); setError("");
      }}><option value="">{t("appDelegations.chooseWorkspace")}</option>{workspaces.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}</select></label>
      <label>{t("appDelegations.assistant")}<select aria-label={t("appDelegations.assistant")} value={targetId} onChange={event => {
        const nextTarget = event.target.value;
        setTargetId(nextTarget);
        if (nextTarget.startsWith("agent:")) setScopes(items => items.filter(scope => NATIVE_SCOPES.has(scope)));
      }} disabled={Boolean(busy) || !workspaceId || loadedWorkspaceId !== workspaceId} required>
        <option value="">{t("appDelegations.chooseAssistant")}</option>
        {agents.length ? <optgroup label={t("appDelegations.nativeAgents")}>{agents.map(item => <option key={item.id} value={`agent:${item.id}`}>{t("appDelegations.nativeAgentName", { name: item.name })}</option>)}</optgroup> : null}
        {definitions.length ? <optgroup label={t("appDelegations.publishedAssistants")}>{definitions.map(item => <option key={item.definitionId} value={`definition:${item.definitionId}`}>{t("appDelegations.publishedAssistantName", { name: item.name })}</option>)}</optgroup> : null}
      </select></label>
      {workspaceId && loadedWorkspaceId === workspaceId && !definitions.length && !agents.length ? <p>{t("appDelegations.noAssistants")}</p> : null}
      <fieldset disabled={Boolean(busy)}><legend>{t("appDelegations.scopes")}</legend>{scopeChoices.map(scope => <label className="appDelegationScope" key={scope}>
        <input type="checkbox" data-scope={scope} aria-label={t(`appDelegations.scope.${scope.replace(":", ".")}`)} checked={scopes.includes(scope)} onChange={event => {
          const checked = event.target.checked;
          setScopes(items => checked ? SCOPES.filter(item => items.includes(item) || item === scope) : items.filter(item => item !== scope));
        }} />
        <span>{t(`appDelegations.scope.${scope.replace(":", ".")}`)}</span>
      </label>)}</fieldset>
      <details className="appDelegationDeveloperDetails"><summary>{t("appDelegations.exactPermissions")}</summary>{scopeChoices.map(scope => <code key={scope} translate="no">{scope}</code>)}</details>
      <label>{t("appDelegations.duration")}<select aria-label={t("appDelegations.duration")} value={duration} onChange={event => setDuration(event.target.value)} disabled={Boolean(busy)}>
        <option value="permanent">{t("appDelegations.permanent")}</option><option value="temporary">{t("appDelegations.temporary")}</option>
      </select>{duration === "permanent" ? <small>{t("appDelegations.permanentHint")}</small> : null}</label>
      {duration === "temporary" ? <label>{t("appDelegations.expiry")}<input aria-label={t("appDelegations.expiry")} type="number" min="300" max="86400" step="1" value={expiry} onChange={event => setExpiry(event.target.value)} disabled={Boolean(busy)} required /><small>{t("appDelegations.expiryHint")}</small></label> : null}
      <button type="submit" disabled={!canAuthorize}>{canAuthorize ? t("appDelegations.consent", { app: app.name, workspace: workspace.name, assistant: targetName, scopes: scopes.map(scope => t(`appDelegations.scope.${scope.replace(":", ".")}`)).join(", "), duration: expiresInSeconds === null ? t("appDelegations.permanent") : t("appDelegations.temporaryDuration", { seconds: expiresInSeconds }) }) : t("appDelegations.authorize")}</button>
    </form>
    {oneTimeToken ? <section className="appDelegationSection" aria-labelledby="delegation-token-heading">
      <h2 id="delegation-token-heading">{t("appDelegations.tokenTitle")}</h2><p>{t("appDelegations.tokenHint")}</p>
      {tokenDelegation ? <p>{tokenDelegation.appName} / {tokenDelegation.workspaceName} / {tokenDelegation.agentName || tokenDelegation.definitionName}<code translate="no">{tokenDelegation.id}</code></p> : null}
      <p>{t("appDelegations.tokenBackendHint")}</p>
      {tokenDelegation?.agentId ? <p>{t("appDelegations.tokenBusinessHint")}</p> : null}
      {oneTimeToken.rotated ? <p role="status">{t("appDelegations.rotationSucceeded")}</p> : null}
      <pre className="appDelegationToken" translate="no">{oneTimeToken.accessToken}</pre>
      <div className="appDelegationActions"><button type="button" onClick={() => void copyToken()}>{t("appDelegations.copyToken")}</button><button type="button" onClick={() => { setOneTimeToken(null); setCopied(false); }}>{t("appDelegations.dismissToken")}</button></div>
      {copied ? <p role="status">{t("appDelegations.copied")}</p> : null}
    </section> : null}
    {!selectedDelegation ? <section className="appDelegationSection" aria-labelledby="app-delegations-heading">
      <h2 id="app-delegations-heading">{t("appDelegations.yourGrants")}</h2>
      {delegations === null ? <p role="status">{t("appDelegations.loading")}</p> : !delegations.length ? <p>{t("appDelegations.noGrants")}</p> : groups.map(([groupId, name]) => <section className="appDelegationGroup" key={groupId}><h3>{name}</h3>{delegations.filter(item => item.appId === groupId).map(item => <article className="appDelegationRow" key={item.id} data-delegation-id={item.id}>
        <div><strong>{item.agentId ? t("appDelegations.nativeAgentName", { name: item.agentName }) : t("appDelegations.publishedAssistantName", { name: item.definitionName })}</strong><small>{item.workspaceName}</small><small>{item.expiresAt === null ? t("appDelegations.permanent") : t("appDelegations.expires", { date: dateText(item.expiresAt) })}</small><small>{item.revokedAt ? t("appDelegations.status.revoked") : activeDelegation(item) ? t("appDelegations.status.active") : t("appDelegations.expired")}</small><code translate="no">{item.id}</code></div>
        <button type="button" data-action="details" disabled={Boolean(busy)} onClick={() => { setSelectedDelegationId(item.id); setDetailTab("overview"); }}>{t("appDelegations.openAuthorization")}</button>
      </article>)}</section>)}
    </section> : <AppDelegationDetails key={selectedDelegation.id} delegation={selectedDelegation} tab={detailTab} onTabChange={setDetailTab} onBack={() => setSelectedDelegationId(null)} canConfigure={workspaces.some(item => item.id === selectedDelegation.workspaceId)} active={activeDelegation(selectedDelegation)} busy={Boolean(busy)} onRotate={rotate} onRevoke={revoke} businessUsers={<BusinessBranches key={selectedDelegation.id} delegation={selectedDelegation} canConfigure={workspaces.some(item => item.id === selectedDelegation.workspaceId)} onClose={() => setDetailTab("overview")} />} />}
    {isSuperuser && !selectedDelegation ? <BusinessAppAdmin onAppsChanged={reloadApps} /> : null}
  </div>;
}
