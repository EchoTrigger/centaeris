import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { createMemoryRouter, RouterProvider } from "react-router";
import AppDelegations from "../../src/routes/AppDelegations";
import SettingsRoute from "../../src/routes/SettingsRoute";
import { configureApi, clearCsrfToken } from "../../src/api";
import { i18n } from "../../src/i18n";

declare global { interface Window { __appDelegationsComplete?: boolean } }
const results: { name: string; actual: unknown; expected: unknown }[] = [];
function check(name: string, actual: unknown, expected: unknown) { results.push({ name, actual, expected }); }
window.addEventListener("error", event => check("browser has no uncaught error", event.message, null));
window.addEventListener("unhandledrejection", event => check("browser has no rejected promise", String(event.reason), null));
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const previewLanguage = new URLSearchParams(location.search).get("previewLanguage");
document.documentElement.dataset.theme = new URLSearchParams(location.search).get("theme") === "dark" ? "dark" : "light";
const settle = async () => { await new Promise(resolve => setTimeout(resolve, 50)); flushSync(() => {}); };
const render = (admin = false) => flushSync(() => root.render(<StrictMode><AppDelegations isSuperuser={admin} /></StrictMode>));
const field = (label: string) => (host.querySelector(`input[data-scope="${label}"]`) || host.querySelector(`input[aria-label="${label}"], select[aria-label="${label}"]`)) as HTMLInputElement | HTMLSelectElement;
function change(label: string, value: string) {
  const element = field(label);
  Object.getOwnPropertyDescriptor(element instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype, "value")!.set!.call(element, value);
  element.dispatchEvent(new Event(element instanceof HTMLSelectElement ? "change" : "input", { bubbles: true }));
}
function button(text: string) {
  const element = [...host.querySelectorAll("button")].find(item => item.textContent?.includes(text));
  if (!element) throw new Error(`Missing button: ${text}`);
  return element;
}
const calls: { path: string; query: string; method: string; body: unknown; credentials: unknown; csrf: string | null; authorization: string | null }[] = [];
const originalFetch = window.fetch;
let copied = "";
let failClipboard = false;
let issueError = "";
let rotateError = "";
let slowDefinitions: ((value: Response) => void) | undefined;
let slowAgents: ((value: Response) => void) | undefined;
let delayDefinitions = false;
let noMembership = false;
let branchMode = "records";
let delayBranchMore = false;
let workMode = "paged";
let delayWorkMore = false;
const slowWork: ((value: Response) => void)[] = [];
const slowBranches: ((value: Response) => void)[] = [];
const branchRecords = (rootAgentId: string) => [
  { branchId: "branch-user-a", rootAgentId, businessUserId: "business-user-a", agentId: "agent-branch-a", sessionId: "session-coordination-a", status: "active", sessionCount: 3, createdAt: "2026-10-01T00:00:00Z" },
  { branchId: "branch-user-b", rootAgentId, businessUserId: "business-user-b", agentId: "agent-branch-b", sessionId: "session-coordination-b", status: "deleted", sessionCount: 1, createdAt: "2026-10-02T00:00:00Z" },
];
const apps = [{ id: "app-report", name: "Reports", status: "active" }];
let adminApps = [...apps, { id: "app-revoked", name: "Retired", status: "revoked" }];
const baseGrant = { id: "grant-orphan", appId: "app-report", appName: "Reports", workspaceId: "workspace-gone", workspaceName: "Former workspace", definitionId: "def-gone" as string | null, definitionName: "Former assistant" as string | null, agentId: null as string | null, agentName: null as string | null, credentialVersion: 1, scopes: ["events:read"], issuer: "centaeris-workspace", audience: "centaeris-workspace-api", createdAt: "2026-09-30T00:00:00Z", expiresAt: "2099-10-01T00:00:00Z" as string | null, revokedAt: null as string | null };
let grants = [baseGrant,
  { ...baseGrant, id: "grant-native", appId: "app-daily", appName: "Daily reports", definitionId: null, definitionName: null, agentId: "agent-daily", agentName: "Daily business agent", credentialVersion: 4, scopes: ["assistant:use", "messages:submit"], expiresAt: null },
  { ...baseGrant, id: "grant-native-alt", appId: "app-daily", appName: "Daily reports", workspaceId: "workspace-research", workspaceName: "Research", definitionId: null, definitionName: null, agentId: "agent-alternative", agentName: "Alternative business agent", scopes: ["assistant:use", "sessions:read"], expiresAt: null },
  { ...baseGrant, id: "grant-expired", definitionName: "Expired assistant", expiresAt: "2000-01-01T00:00:00Z" },
  { ...baseGrant, id: "grant-revoked", definitionName: "Revoked assistant", revokedAt: "2026-09-30T01:00:00Z" },
];
const response = (body: unknown, status = 200) => new Response(status === 204 ? null : JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
window.fetch = async (input, options = {}) => {
  const url = new URL(String(input));
  const path = url.pathname.replace(/^\/deployment-prefix/, "");
  const method = options.method ?? "GET";
  const headers = new Headers(options.headers);
  const body = options.body ? JSON.parse(String(options.body)) : null;
  calls.push({ path, query: url.search, method, body, credentials: options.credentials, csrf: headers.get("X-CSRFToken"), authorization: headers.get("Authorization") });
  if (path === "/api/csrf") return response({ csrfToken: "fixture-csrf" });
  if (path === "/api/business-apps") return response({ apps: apps.filter(app => app.status === "active") });
  if (path === "/api/workspaces") return response({ workspaces: noMembership ? [] : [{ id: "workspace-research", name: "Research", role: "member" }, { id: "workspace-support", name: "Support", role: "member" }] });
  if (path.includes("available-agent-definitions")) {
    if (delayDefinitions && path.includes("workspace-research")) return new Promise(resolve => { slowDefinitions = resolve; });
    return response({ definitions: path.includes("workspace-research") ? [{ id: "version-research", definitionId: "def-research", name: "Research assistant" }] : [{ id: "version-support", definitionId: "def-support", name: "Support assistant" }] });
  }
  if (/\/workspaces\/[^/]+\/agents$/.test(path)) {
    if (delayDefinitions && path.includes("workspace-research")) return new Promise(resolve => { slowAgents = resolve; });
    return response({ agents: [{ id: path.includes("workspace-research") ? "agent-research" : "agent-support", name: path.includes("workspace-research") ? "Research Agent" : "Support Agent", definitionId: null, status: "active" }, { id: "agent-managed", name: "Managed incarnation", definitionId: "def-research", status: "active" }] });
  }
  if (/^\/api\/agents\/[^/]+\/business-branches$/.test(path) && method === "GET") {
    if (branchMode === "failure") return response({ error: "business_branch_not_found" }, 404);
    if (branchMode === "delayed") return new Promise(resolve => { slowBranches.push(resolve); });
    const rows = branchRecords(path.split("/")[3]);
    if (branchMode === "paged") {
      if (!url.searchParams.has("afterBranchId")) return response({ branches: [rows[0]], nextAfterId: "branch-user-a" });
      if (delayBranchMore) return new Promise(resolve => { slowBranches.push(resolve); });
      return response({ branches: rows, nextAfterId: null });
    }
    return response({ branches: branchMode === "empty" ? [] : rows, nextAfterId: null });
  }
  if (/^\/api\/agents\/[^/]+\/business-branches\/[^/]+\/sessions$/.test(path) && method === "GET") {
    if (workMode === "delayed" || (url.searchParams.has("afterSessionId") && delayWorkMore)) return new Promise(resolve => { slowWork.push(resolve); });
    if (workMode === "failure") return response({ error: "business_branch_not_found" }, 404);
    const work = [{ sessionId: "session-work-a", title: "Verify customer request", status: "active", sourceAgentRunId: "run-source-a", createdAt: "2026-10-02T01:00:00Z" }, { sessionId: "session-work-b", title: "Prepare customer report", status: "deleted", sourceAgentRunId: "run-source-b", createdAt: "2026-10-02T02:00:00Z" }];
    return response({ branchId: path.split("/")[5], coordinationSession: { sessionId: "session-coordination-a", title: "Customer coordination", status: "active", createdAt: "2026-10-01T00:00:00Z" }, workSessions: workMode === "empty" ? [] : url.searchParams.has("afterSessionId") ? work : [work[0]], nextAfterSessionId: workMode === "empty" || url.searchParams.has("afterSessionId") ? null : "session-work-a" });
  }
  if (path === "/api/account/app-delegations" && method === "GET") return response({ delegations: grants });
  if (path === "/api/account/app-delegations" && method === "POST") {
    if (issueError) return response({ error: issueError }, 400);
    const delegation = { ...baseGrant, id: `grant-new-${grants.length}`, workspaceId: body.workspaceId, workspaceName: "Research", definitionId: body.definitionId ?? null, definitionName: body.definitionId ? "Research assistant" : null, agentId: body.agentId ?? null, agentName: body.agentId ? "Research Agent" : null, scopes: body.scopes, expiresAt: body.expiresInSeconds === null ? null : "2099-10-01T00:00:00Z" };
    grants = [...grants, delegation];
    return response({ delegation, accessToken: "fixture-one-time-token", tokenType: "Bearer" }, 201);
  }
  if (path.endsWith("/rotate") && method === "POST") {
    if (rotateError) {
      grants = grants.map(grant => grant.id === "grant-native" ? { ...grant, credentialVersion: grant.credentialVersion + 1 } : grant);
      const error = rotateError; rotateError = "";
      return response({ error }, 409);
    }
    const current = grants.find(grant => grant.id === path.split("/").at(-2))!;
    if (body.expectedCredentialVersion !== current.credentialVersion) return response({ error: "delegation_credential_conflict" }, 409);
    const delegation = { ...current, credentialVersion: current.credentialVersion + 1 };
    grants = grants.map(grant => grant.id === delegation.id ? delegation : grant);
    return response({ delegation, accessToken: `fixture-rotated-token-${delegation.credentialVersion}`, tokenType: "Bearer" });
  }
  if (path.startsWith("/api/account/app-delegations/") && method === "DELETE") {
    grants = grants.map(grant => grant.id === path.split("/").at(-1) ? { ...grant, revokedAt: "2026-09-30T01:00:00Z" } : grant);
    return response(null, 204);
  }
  if (path === "/api/admin/business-apps" && method === "GET") return response({ apps: adminApps });
  if (path === "/api/admin/business-apps" && method === "POST") {
    const app = { id: "app-new", name: body.name, status: "pending" };
    adminApps = [...adminApps, app];
    return response({ app }, 201);
  }
  if (path.startsWith("/api/admin/business-apps/") && method === "PATCH") {
    const app = { ...adminApps.find(item => item.id === path.split("/").at(-1))!, status: body.status };
    adminApps = adminApps.map(item => item.id === app.id ? app : item);
    return response({ app });
  }
  throw new Error(`Unexpected fixture request: ${method} ${path}`);
};
Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: async (value: string) => { if (failClipboard) throw new Error("Clipboard denied"); copied = value; } } });
configureApi({ apiBaseUrl: "https://fixture.invalid/deployment-prefix" });
clearCsrfToken();
try {
  await i18n.changeLanguage("en");
  const storageBefore = JSON.stringify({ local: { ...localStorage }, session: { ...sessionStorage } });
  render(); await settle();
  check("new authorization is a separate explicit action", Boolean(host.querySelector('[data-action="new-authorization"]')), true);
  check("authorization form starts collapsed", host.querySelector(".appDelegationConsent")?.hasAttribute("hidden"), true);
  check("authorization entry is bound to grant identity", Boolean(host.querySelector('[data-delegation-id="grant-native"] [data-action="details"]')), true);
  button("New authorization").click(); await settle();
  check("member does not call administrator API", calls.some(call => call.path.startsWith("/api/admin/")), false);
  check("lost membership grant remains visible", host.textContent!.includes("Former workspace"), true);
  check("consent disabled without explicit choices", button("Authorize").disabled, true);
  check("no scopes selected by default", host.querySelectorAll('input[type="checkbox"]:checked').length, 0);
  check("all eight supported scopes offered", host.querySelectorAll('input[type="checkbox"]').length, 8);
  check("permanent duration is the explicit default", field("Authorization duration")?.value, "permanent");
  const row = (name: string) => [...host.querySelectorAll("article")].find(item => item.textContent!.includes(name))!;
  check("permanent native grant has readable duration", row("Daily business agent")?.textContent!.includes("Until revoked"), true);
  check("permanent grant does not display Invalid Date or epoch", /Invalid Date|1970/.test(host.textContent!), false);
  const openGrant = async (name: string, tab = "overview") => {
    const selected = host.querySelector(".appDelegationDetails");
    if (selected && !selected.querySelector(".appDelegationDetailHeader")?.textContent?.includes(name)) { button("Back to applications").click(); await settle(); }
    if (!host.querySelector(".appDelegationDetails")) { row(name).querySelector<HTMLButtonElement>('[data-action="details"]')!.click(); await settle(); }
    host.querySelector<HTMLButtonElement>(`[role="tab"][data-tab="${tab}"]`)!.click(); await settle();
  };
  const back = async () => { button("Back to applications").click(); await settle(); };
  check("applications group multiple independent authorizations", [host.querySelectorAll(".appDelegationGroup").length, host.querySelectorAll('[data-delegation-id="grant-native"], [data-delegation-id="grant-native-alt"]').length], [2, 2]);
  await openGrant("Alternative business agent", "api");
  check("same application second authorization gets its own root guide", [host.querySelector(".appDelegationDetails")?.getAttribute("data-selected-delegation-id"), host.querySelector(".appDelegationEndpoint")?.textContent], ["grant-native-alt", "POST https://fixture.invalid/deployment-prefix/api/v1/chats"]); await back();
  await openGrant("Daily business agent", "credentials");
  check("active native grant has rotation action", Boolean(host.querySelector(".appDelegationDetailPanel")?.textContent!.includes("Rotate token")), true); await back();
  await openGrant("Expired assistant", "credentials");
  check("expired grant has no rotation action", host.querySelector(".appDelegationDetailPanel")?.textContent!.includes("Rotate token"), false); await back();
  await openGrant("Revoked assistant", "credentials");
  check("revoked grant has no rotation action", host.querySelector(".appDelegationDetailPanel")?.textContent!.includes("Rotate token"), false); await back();
  change("Application", "app-report"); change("Workspace", "workspace-research"); await settle();
  check("owned Agent endpoint loaded", calls.some(call => call.path === "/api/workspaces/workspace-research/agents"), true);
  check("native Agents and published assistants are grouped", field("Agent or published assistant")?.querySelectorAll("optgroup").length, 2);
  check("managed incarnations are excluded from native options", host.textContent!.includes("Managed incarnation"), false);
  change("Agent or published assistant", "agent:agent-research"); await settle();
  check("native Agent offers explicit upload and download permissions", [...host.querySelectorAll('input[type="checkbox"]')].map(item => item.getAttribute("data-scope")), ["assistant:use", "sessions:read", "messages:submit", "attachments:write", "artifacts:read"]);
  (field("assistant:use") as HTMLInputElement).click(); (field("sessions:read") as HTMLInputElement).click(); await settle();
  (field("messages:submit") as HTMLInputElement).click(); (field("artifacts:read") as HTMLInputElement).click(); await settle();
  for (const text of ["Reports", "Research", "Research Agent", "assistant:use", "sessions:read", "messages:submit", "artifacts:read", "Until revoked"]) check(`permanent consent names ${text}`, button("Authorize").textContent!.includes(text.includes(":") ? String(i18n.t(`appDelegations.scope.${text.replace(":", ".")}`)) : text), true);
  button("Authorize").click(); await settle();
  check("native permanent request includes actual artifact delivery and explicit null expiry", calls.find(call => call.path === "/api/account/app-delegations" && call.method === "POST")?.body, { appId: "app-report", workspaceId: "workspace-research", agentId: "agent-research", scopes: ["assistant:use", "sessions:read", "messages:submit", "artifacts:read"], expiresInSeconds: null });
  check("token explanation confines credential to trusted backend", host.textContent!.includes("trusted business backend"), true);
  check("token explanation requires stable authenticated external user identifier", host.textContent!.includes("stable external user identifier"), true);
  check("token explanation puts chats, memory and files in each branch", host.textContent!.includes("conversations, memory, files and artifacts"), true);
  await openGrant("Daily business agent");
  check("native details expose four focused entries", [...host.querySelectorAll('[role="tab"]')].map(item => item.getAttribute("data-tab")), ["overview", "api", "users", "credentials"]);
  check("detail selects the authorization record rather than consent target", host.querySelector(".appDelegationDetails")?.getAttribute("data-selected-delegation-id"), "grant-native");
  check("detail header identifies the selected target", host.querySelector(".appDelegationDetailHeader")?.textContent!.includes("Daily business agent"), true);
  host.querySelector<HTMLButtonElement>('[data-tab="api"]')!.click(); await settle();
  check("native quick start has four actual contract steps", host.querySelectorAll(".appDelegationGuideSteps > li").length, 4);
  check("API guide preserves public deployment prefix and selected root", host.querySelector(".appDelegationEndpoint")?.textContent, "POST https://fixture.invalid/deployment-prefix/api/v1/chats");
  check("guide excludes unsaved consent target", host.querySelector(".appDelegationGuide")?.textContent!.includes("agent-research"), false);
  check("guide never embeds one-time token", host.querySelector(".appDelegationGuide")?.textContent!.includes("fixture-one-time-token"), false);
  check("Dify identity and message stream semantics are explicit", ["sys.user_id", "sys.conversation_id", "nextCursor", "Last-Event-ID", "Idempotency-Key", "message.updated"].every(text => host.querySelector(".appDelegationGuide")?.textContent!.includes(text)), true);
  for (const language of ["Python", "JavaScript", "cURL"]) { button(language).click(); await settle(); check(`${language} backend example uses selected root`, host.querySelector(".appDelegationExample")?.textContent!.includes("agent-daily"), true); }
  button("Copy example").click(); await settle(); check("copying example cannot copy actual token", copied.includes("fixture-one-time-token"), false);
  failClipboard = true; button("Example copied").click(); await settle();
  check("failed example copy has accessible feedback", host.querySelector(".appDelegationGuide [role='alert']")?.textContent, "Unable to copy the example. Select the code and copy it manually.");
  failClipboard = false;
  const overviewTab = host.querySelector<HTMLButtonElement>('[data-tab="overview"]')!;
  overviewTab.click(); await settle(); overviewTab.focus(); overviewTab.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true })); await settle();
  check("keyboard navigation selects and focuses the next tab", [host.querySelector('[role="tab"][aria-selected="true"]')?.getAttribute("data-tab"), document.activeElement?.getAttribute("data-tab")], ["api", "api"]);
  check("only selected tab is a tab stop", host.querySelectorAll('[role="tab"][tabindex="0"]').length, 1);
  check("tab panel is labelled by its active tab", host.querySelector('[role="tabpanel"]')?.getAttribute("aria-labelledby"), "delegation-tab-api");
  check("native grant exposes branch maintenance", Boolean(host.querySelector('[data-tab="users"]')), true);
  await back(); await openGrant("Former assistant");
  host.querySelector<HTMLButtonElement>('[data-tab="api"]')!.click(); await settle();
  check("published assistant displays the same four-step guide", host.querySelectorAll(".appDelegationGuideSteps > li").length, 4);
  check("published guide creates a chat for the selected assistant", [host.querySelector(".appDelegationGuide")?.textContent!.includes("def-gone"), host.querySelector(".appDelegationEndpoint")?.textContent], [true, "POST https://fixture.invalid/deployment-prefix/api/v1/chats"]);
  check("published assistant has no branch maintenance action", Boolean(host.querySelector('[data-tab="users"]')), false); await back();
  await openGrant("Former assistant", "credentials");
  check("published credentials do not claim native branch execution semantics", host.querySelector(".appDelegationDetailPanel")?.textContent!.includes("existing branch identities"), false); await back();
  branchMode = "delayed"; await openGrant("Daily business agent", "users");
  check("branch lookup displays loading state", host.querySelector(".appDelegationBranchView [role='status']")?.textContent, "Loading isolated branches…");
  const branchCall = calls.filter(call => call.path.endsWith("/business-branches")).at(-1)!;
  check("branch lookup uses grant root and app rather than selected consent fields", [branchCall?.path, branchCall?.query], ["/api/agents/agent-daily/business-branches", "?appId=app-daily"]);
  check("owner branch maintenance uses browser cookie without delegated bearer", [branchCall?.credentials, branchCall?.authorization], ["include", null]);
  for (const resolve of slowBranches.splice(0)) resolve(response({ branches: branchRecords("agent-daily"), nextAfterId: null })); await settle();
  check("two distinct external user branches are visible", host.querySelectorAll(".appDelegationBranch").length, 2);
  for (const text of ["business-user-a", "business-user-b", "agent-branch-a", "agent-branch-b", "session-coordination-a", "session-coordination-b", "Deleted", "3"]) check(`branch maintenance shows ${text}`, host.querySelector(".appDelegationBranchView")?.textContent!.includes(text), true);
  const expandWork = () => host.querySelector<HTMLButtonElement>(".appDelegationBranchToggle")!.click();
  check("work relationships are loaded lazily", calls.some(call => call.path.endsWith("/business-branches/branch-user-a/sessions")), false);
  expandWork(); await settle();
  check("actual work lookup is scoped to selected root app and branch", [calls.filter(call => call.path.endsWith("/sessions")).at(-1)?.path, calls.filter(call => call.path.endsWith("/sessions")).at(-1)?.query], ["/api/agents/agent-daily/business-branches/branch-user-a/sessions", "?appId=app-daily"]);
  check("work tree displays authoritative coordination and child metadata", ["Customer coordination", "Verify customer request", "session-work-a", "run-source-a"].every(text => host.querySelector(".appDelegationSessionTree")?.textContent!.includes(text)), true);
  check("retained session count does not manufacture child sessions", host.querySelectorAll(".appDelegationWorkSession").length, 1);
  delayWorkMore = true; button("Load more work sessions").click(); await settle();
  check("loading work page keeps existing relationships", [host.querySelectorAll(".appDelegationWorkSession").length, host.querySelector(".appDelegationSessionTree [role='status']")?.textContent], [1, "Loading more work sessions…"]);
  for (const resolve of slowWork.splice(0)) resolve(response({ error: "business_branch_not_found" }, 404)); await settle();
  check("failed work page retains existing relationships and offers retry", [host.querySelectorAll(".appDelegationWorkSession").length, Boolean(host.querySelector(".appDelegationSessionTree [role='alert']"))], [1, true]);
  delayWorkMore = false; button("Retry more work sessions").click(); await settle();
  check("work next page preserves root app branch and actual session cursor", [calls.filter(call => call.path.endsWith("/sessions")).at(-1)?.path, calls.filter(call => call.path.endsWith("/sessions")).at(-1)?.query], ["/api/agents/agent-daily/business-branches/branch-user-a/sessions", "?appId=app-daily&afterSessionId=session-work-a"]);
  check("work pages deduplicate by session ID", [...host.querySelectorAll(".appDelegationWorkSession > code")].map(item => item.textContent), ["session-work-a", "session-work-b"]);
  check("terminal work page removes further loading action", [...host.querySelectorAll("button")].some(item => item.textContent === "Load more work sessions"), false);
  expandWork(); await settle(); workMode = "empty"; expandWork(); await settle();
  check("empty work tree retains actual coordination without invented children", [host.querySelectorAll(".appDelegationCoordinationSession").length, host.querySelectorAll(".appDelegationWorkSession").length, host.querySelector(".appDelegationSessionTree")?.textContent!.includes("No child work sessions")], [1, 0, true]);
  expandWork(); await settle(); workMode = "failure"; expandWork(); await settle();
  check("failed initial work lookup has local retry", Boolean(host.querySelector(".appDelegationSessionTree [role='alert']")), true);
  workMode = "paged"; button("Retry loading sessions").click(); await settle();
  check("retry reads actual work without changing selected authorization", [host.querySelectorAll(".appDelegationWorkSession").length, host.querySelector(".appDelegationDetails")?.getAttribute("data-selected-delegation-id")], [1, "grant-native"]);
  expandWork(); await settle(); workMode = "delayed"; expandWork(); await settle();
  button("Close branch list").click(); await settle();
  for (const resolve of slowWork.splice(0)) resolve(response({ branchId: "branch-user-a", coordinationSession: { sessionId: "session-coordination-a", title: "Late coordination", status: "active", createdAt: "2026-10-01T00:00:00Z" }, workSessions: [], nextAfterSessionId: null })); await settle();
  check("late work result cannot reopen a closed branch tree", host.querySelector(".appDelegationSessionTree"), null); workMode = "paged";
  check("closing branch maintenance clears the view", host.querySelector(".appDelegationBranchView"), null);
  check("branch maintenance keeps consent choices", [field("Application").value, field("Workspace").value, field("Agent or published assistant").value, field("Authorization duration").value], ["app-report", "workspace-research", "agent:agent-research", "permanent"]);
  check("branch maintenance keeps explicit native scope choices", [...host.querySelectorAll('input[type="checkbox"]:checked')].map(item => item.getAttribute("data-scope")), ["assistant:use", "sessions:read", "messages:submit", "artifacts:read"]);
  branchMode = "paged"; await openGrant("Daily business agent", "users");
  check("branch first page shows only its first user", host.querySelectorAll(".appDelegationBranch").length, 1);
  check("branch cursor exposes load more action", Boolean([...host.querySelectorAll("button")].find(item => item.textContent === "Load more branches")), true);
  delayBranchMore = true; button("Load more branches").click(); await settle();
  check("loading branch page retains already visible user", host.querySelectorAll(".appDelegationBranch").length, 1);
  check("loading branch page provides status", host.querySelector(".appDelegationBranchView [role='status']")?.textContent, "Loading more branches…");
  for (const resolve of slowBranches.splice(0)) resolve(response({ error: "business_branch_not_found" }, 404)); await settle();
  check("failed additional page keeps existing branch records", host.querySelectorAll(".appDelegationBranch").length, 1);
  check("failed additional page exposes retry state", host.querySelector(".appDelegationBranchView [role='alert']")?.textContent, "Unable to load more branches. Try again.");
  delayBranchMore = false; button("Retry loading more").click(); await settle();
  check("next page retains original root app and branch cursor", [calls.filter(call => call.path.endsWith("/business-branches")).at(-1)?.path, calls.filter(call => call.path.endsWith("/business-branches")).at(-1)?.query], ["/api/agents/agent-daily/business-branches", "?appId=app-daily&afterBranchId=branch-user-a"]);
  check("merged branch pages deduplicate by branch identity", [...host.querySelectorAll(".appDelegationBranch")].map(item => item.textContent!.includes("business-user-a") ? "a" : "b"), ["a", "b"]);
  check("terminal branch cursor removes further paging action", [...host.querySelectorAll("button")].some(item => item.textContent === "Load more branches" || item.textContent === "Retry loading more"), false);
  check("readonly branch paging preserves one-time token", host.querySelector(".appDelegationToken")?.textContent, "fixture-one-time-token");
  button("Close branch list").click(); await settle();
  branchMode = "empty"; await openGrant("Daily business agent", "users");
  check("branch maintenance has a useful empty state", host.querySelector(".appDelegationBranchView")?.textContent!.includes("No isolated branches have been created for this application."), true);
  button("Close branch list").click(); await settle();
  branchMode = "failure"; await openGrant("Daily business agent", "users");
  check("branch lookup failure stays inside readonly view", host.querySelector(".appDelegationBranchView [role='alert']")?.textContent, "Unable to load isolated branches. Check that you can still access this root Agent and application.");
  check("failed lookup exposes no stale branch records", host.querySelectorAll(".appDelegationBranch").length, 0);
  button("Close branch list").click(); await settle();
  branchMode = "delayed"; await openGrant("Daily business agent", "users"); button("Close branch list").click(); await settle();
  for (const resolve of slowBranches.splice(0)) resolve(response({ branches: branchRecords("agent-daily"), nextAfterId: null })); await settle();
  check("late branch response cannot reopen a closed view", host.querySelector(".appDelegationBranchView"), null);
  check("branch maintenance never resolves, creates or messages a branch", calls.filter(call => call.path.includes("business-branches")).every(call => call.method === "GET"), true);
  branchMode = "records";
  button("Dismiss token").click(); await settle();
  await openGrant("Daily business agent", "credentials");
  const rotate = () => button("Rotate token");
  rotate().click(); await settle();
  check("rotation sends observed credential version", calls.find(call => call.path.endsWith("grant-native/rotate"))?.body, { expectedCredentialVersion: 4 });
  check("replacement token shown once", host.textContent!.includes("fixture-rotated-token-5"), true);
  check("rotation explains old token invalidation", host.textContent!.includes("The previous token is now invalid"), true);
  button("Copy token").click(); await settle(); check("copy receives replacement token", copied, "fixture-rotated-token-5");
  rotate().click(); await settle();
  check("second rotation uses returned current version", calls.filter(call => call.path.endsWith("grant-native/rotate")).at(-1)?.body, { expectedCredentialVersion: 5 });
  check("replacement clears previously displayed token", host.textContent!.includes("fixture-rotated-token-5"), false);
  check("second replacement displayed", host.textContent!.includes("fixture-rotated-token-6"), true);
  const readsBeforeConflict = calls.filter(call => call.path === "/api/account/app-delegations" && call.method === "GET").length;
  rotateError = "delegation_credential_conflict"; rotate().click(); await settle();
  check("rotation conflict readable", host.querySelector('[role="alert"]')?.textContent!.includes("changed in another request"), true);
  check("failed rotation displays no token", host.querySelector(".appDelegationToken"), null);
  check("conflict rereads authorizations on the same page", calls.filter(call => call.path === "/api/account/app-delegations" && call.method === "GET").length, readsBeforeConflict + 1);
  check("conflict keeps application and workspace choices", [field("Application").value, field("Workspace").value], ["app-report", "workspace-research"]);
  check("conflict keeps target and lifetime choices", [field("Agent or published assistant").value, field("Authorization duration").value], ["agent:agent-research", "permanent"]);
  check("conflict keeps all selected native permissions", [...host.querySelectorAll('input[type="checkbox"]:checked')].map(item => item.getAttribute("data-scope")), ["assistant:use", "sessions:read", "messages:submit", "artifacts:read"]);
  rotateError = "";
  rotate().click(); await settle();
  check("retry uses reread credential version", calls.filter(call => call.path.endsWith("grant-native/rotate")).at(-1)?.body, { expectedCredentialVersion: 7 });
  check("retry after conflict issues the replacement token", host.textContent!.includes("fixture-rotated-token-8"), true);
  button("Dismiss token").click(); await settle();
  await back();
  (field("messages:submit") as HTMLInputElement).click(); (field("artifacts:read") as HTMLInputElement).click(); await settle();
  change("Agent or published assistant", "definition:def-research"); await settle();
  check("published assistant retains eight scopes", host.querySelectorAll('input[type="checkbox"]').length, 8);
  (field("events:read") as HTMLInputElement).click(); await settle();
  change("Agent or published assistant", "agent:agent-research"); await settle();
  check("native switch clears unsupported selected scopes", [...host.querySelectorAll('input[type="checkbox"]:checked')].map(item => item.getAttribute("data-scope")), ["assistant:use", "sessions:read"]);
  change("Agent or published assistant", "definition:def-research"); change("Authorization duration", "temporary"); await settle();
  check("temporary duration retains one hour default", field("Expiry (seconds)").value, "3600");
  const consent = button("Authorize");
  for (const text of ["Reports", "Research", "Research assistant", "assistant:use", "sessions:read", "3600"]) check(`consent names ${text}`, consent.textContent!.includes(text.includes(":") ? String(i18n.t(`appDelegations.scope.${text.replace(":", ".")}`)) : text), true);
  change("Expiry (seconds)", "299"); await settle(); check("too short expiry disabled", button("Authorize").disabled, true);
  change("Expiry (seconds)", "86401"); await settle(); check("too long expiry disabled", button("Authorize").disabled, true);
  change("Expiry (seconds)", "3600"); await settle();
  button("Authorize").click(); await settle();
  const issued = calls.filter(call => call.path === "/api/account/app-delegations" && call.method === "POST").at(-1)!;
  check("issued exact camelCase consent request", issued.body, { appId: "app-report", workspaceId: "workspace-research", definitionId: "def-research", scopes: ["assistant:use", "sessions:read"], expiresInSeconds: 3600 });
  check("mutation uses browser cookie", issued.credentials, "include");
  check("mutation uses CSRF", issued.csrf, "fixture-csrf");
  check("issued token shown once", host.textContent!.includes("fixture-one-time-token"), true);
  button("Copy token").click(); await settle(); check("copy receives token", copied, "fixture-one-time-token");
  check("token never stored", JSON.stringify({ local: { ...localStorage }, session: { ...sessionStorage } }), storageBefore);
  button("Dismiss token").click(); await settle(); check("dismiss clears token", host.textContent!.includes("fixture-one-time-token"), false);
  await openGrant("Former assistant", "credentials");
  button("Revoke authorization").click(); await settle();
  check("orphan grant revoked with account endpoint", calls.some(call => call.path === "/api/account/app-delegations/grant-orphan" && call.method === "DELETE" && call.csrf === "fixture-csrf"), true);
  check("orphan revoke updates visible status", host.querySelector(".appDelegationDetailPanel")?.textContent!.includes("Revoked"), true); await back();
  issueError = "delegation_not_available"; button("Authorize").click(); await settle();
  check("unavailable authorization error readable", host.querySelector('[role="alert"]')?.textContent!.includes("no longer available"), true);
  issueError = "";
  flushSync(() => root.render(null)); render(); await settle();
  check("remount never restores token", host.textContent!.includes("fixture-one-time-token"), false);
  button("New authorization").click(); await settle();
  delayDefinitions = true;
  change("Workspace", "workspace-research"); await settle(); change("Workspace", "workspace-support"); await settle();
  slowDefinitions?.(response({ definitions: [{ id: "old-version", definitionId: "old-def", name: "Stale assistant" }] })); await settle();
  slowAgents?.(response({ agents: [{ id: "old-agent", name: "Stale Agent", definitionId: null }] })); await settle();
  check("stale workspace responses ignored", host.textContent!.includes("Stale assistant") || host.textContent!.includes("Stale Agent"), false);
  check("current workspace definitions retained", field("Agent or published assistant").textContent!.includes("Support assistant"), true);
  check("current workspace native Agent retained", field("Agent or published assistant").textContent!.includes("Support Agent"), true);
  render(true); await settle();
  change("Application name", "New integration"); await settle(); button("Register application").click(); await settle();
  const newRow = () => [...host.querySelectorAll("article")].find(row => row.textContent!.includes("New integration"))!;
  check("registration starts pending", newRow().textContent!.includes("Pending"), true);
  (newRow().querySelector('button[data-action="activate"]') as HTMLButtonElement).click(); await settle();
  check("activation visible", newRow().textContent!.includes("Active"), true);
  (newRow().querySelector('button[data-action="revoke"]') as HTMLButtonElement).click(); await settle();
  check("application revoke visible", newRow().textContent!.includes("Revoked"), true);
  check("revoked application has no activation action", newRow().querySelector('button[data-action="activate"]'), null);
  check("all mutations cookie and CSRF authenticated", calls.filter(call => !["GET", "HEAD"].includes(call.method)).every(call => call.credentials === "include" && call.csrf === "fixture-csrf"), true);
  check("UI never uses delegated bearer token for settings", calls.every(call => call.authorization === null), true);
  flushSync(() => root.render(null));
  noMembership = true;
  const router = createMemoryRouter([{ id: "authenticated", HydrateFallback: () => null, loader: () => ({ user: { id: "user-fixture", isSuperuser: false } }), children: [{ path: "/settings/applications", loader: () => ({ workspace: null, workspaces: [] }), Component: SettingsRoute }] }], { initialEntries: ["/settings/applications"] });
  flushSync(() => root.render(<RouterProvider router={router} />)); await settle(); await settle();
  check("account Applications opens without membership", host.querySelector('[role="dialog"]')?.getAttribute("aria-label"), "Applications");
  check("account navigation identifies Applications", host.querySelector('a[aria-current="page"]')?.getAttribute("href"), "/settings/applications");
  check("orphaned grants accessible from account Settings", host.textContent!.includes("Former workspace"), true);
  check("no workspace needed to list authorizations", field("Workspace").querySelectorAll("option").length, 1);
  check("application settings content keeps usable mobile width", innerWidth > 700 || host.querySelector(".workspaceSettingsContent")!.getBoundingClientRect().width >= 300, true);
  router.dispose();
  if (previewLanguage) {
    noMembership = false; delayDefinitions = false;
    flushSync(() => root.render(null));
    await i18n.changeLanguage(previewLanguage);
    const previewRouter = createMemoryRouter([{ id: "authenticated", HydrateFallback: () => null, loader: () => ({ user: { id: "user-fixture", isSuperuser: false } }), children: [{ path: "/settings/applications", loader: () => ({ workspace: { id: "workspace-research", name: "Research", role: "member" }, workspaces: [] }), Component: SettingsRoute }] }], { initialEntries: ["/settings/applications"] });
    flushSync(() => root.render(<RouterProvider router={previewRouter} />)); await settle(); await settle();
    const text = (key: string) => String(i18n.t(`appDelegations.${key}`));
    button(text("newAuthorization")).click(); await settle();
    change(text("application"), "app-report"); change(text("workspace"), "workspace-research"); await settle();
    change(text("assistant"), "agent:agent-research"); await settle();
    for (const scope of ["assistant:use", "sessions:read", "messages:submit", "artifacts:read"]) (field(scope) as HTMLInputElement).click();
    await settle();
    check("localized permanent duration remains explicit default", field(text("duration")).value, "permanent");
    check("localized consent names permanent duration", host.querySelector('form.appDelegationSection > button')?.textContent!.includes(text("permanent")), true);
    check("localized consent distinguishes business Agent", host.querySelector('form.appDelegationSection > button')?.textContent!.includes(String(i18n.t("appDelegations.nativeAgentName", { name: "Research Agent" }))), true);
    (host.querySelector('form.appDelegationSection > button') as HTMLButtonElement).click(); await settle();
    check("localized native issue uses permanent contract", calls.filter(call => call.path === "/api/account/app-delegations" && call.method === "POST").at(-1)?.body, { appId: "app-report", workspaceId: "workspace-research", agentId: "agent-research", scopes: ["assistant:use", "sessions:read", "messages:submit", "artifacts:read"], expiresInSeconds: null });
    check("localized issue displays a one-time token", host.querySelector(".appDelegationToken")?.textContent, "fixture-one-time-token");
    check("localized token not stored", JSON.stringify({ local: { ...localStorage }, session: { ...sessionStorage } }).includes("fixture-one-time-token"), false);
    button(text("dismissToken")).click(); await settle();
    const nativeRow = [...host.querySelectorAll("article")].find(item => item.textContent!.includes(String(i18n.t("appDelegations.nativeAgentName", { name: "Research Agent" }))))!;
    nativeRow.querySelector<HTMLButtonElement>('[data-action="details"]')!.click(); await settle();
    host.querySelector<HTMLButtonElement>('[data-tab="users"]')!.click(); await settle();
    check("localized owner branch tree shows both users", host.querySelectorAll(".appDelegationBranch").length, 2);
    check("root configuration reuses existing verified settings route", host.querySelector(".appDelegationBranchView a")?.getAttribute("href"), "/w/workspace-research/settings/agents?agentId=agent-research");
    host.querySelector<HTMLButtonElement>(".appDelegationBranchToggle")!.click(); await settle();
    check("localized actual child tree uses retained relationship", host.querySelectorAll(".appDelegationWorkSession").length, 1);
    check("settings navigation retains its application icon", Boolean(host.querySelector('.workspaceSettingsNav a[aria-current="page"] svg')), true);
    check("actual dialog retains readable application content width", innerWidth > 700 || host.querySelector(".workspaceSettingsContent")!.getBoundingClientRect().width >= 300, true);
    check("settings page has no horizontal overflow", document.documentElement.scrollWidth <= innerWidth, true);
  }
} catch (error) {
  check("fixture completed", { error: error instanceof Error ? error.message : String(error), content: host.textContent, calls }, "success");
} finally {
  window.fetch = originalFetch;
  if (!previewLanguage) flushSync(() => root.unmount());
  document.getElementById("results")!.textContent = JSON.stringify(results);
  window.__appDelegationsComplete = true;
}
