import http from "node:http";

const agents = [
  { id: "centaeris", workspaceId: "ws_1", name: "Centaeris", description: "私人 Agent", instructions: "保持判断清晰，先核验事实，再回答。", avatarKind: "centaeris", status: "active", deletedAt: null, createdAt: "2026-08-01T00:00:00Z", updatedAt: "2026-08-01T00:00:00Z" },
  { id: "research", workspaceId: "ws_1", name: "研究助手", description: "梳理资料与形成结论", instructions: "优先检索一手资料，区分事实、推断与未知。", avatarKind: "centaeris", status: "active", deletedAt: null, createdAt: "2026-08-02T00:00:00Z", updatedAt: "2026-08-02T00:00:00Z" },
  { id: "writer", workspaceId: "ws_1", name: "写作伙伴", description: "起草、改写与校对", instructions: "保留作者语气，先处理结构，再润色句子。", avatarKind: "centaeris", status: "active", deletedAt: null, createdAt: "2026-08-03T00:00:00Z", updatedAt: "2026-08-03T00:00:00Z" },
];
const plugins = [
  { name: "banana", displayName: "Banana Extension", shortDescription: "Synthetic extension fixture.", capabilities: ["Synthetic capability"], version: "1.0.0", packageDigest: `sha256:${"a".repeat(64)}`, enabled: true, skills: [{ path: "skills/banana/SKILL.md", digest: `sha256:${"b".repeat(64)}` }], cli: [{ path: "bin/banana", digest: `sha256:${"c".repeat(64)}` }], mcpServers: [{ id: "banana-source", transport: { type: "streamableHttp", endpoint: "https://banana.invalid/mcp" }, auth: { type: "bearer", credentialRef: "banana-token", credentialConfigured: true }, tools: [] }], hooks: [] },
  { name: "data-workspace", displayName: "Data Workspace", shortDescription: "Query structured data from the workspace.", capabilities: ["SQL", "Data analysis"], version: "1.0.0", packageDigest: `sha256:${"f".repeat(64)}`, enabled: false, skills: [], cli: [], mcpServers: [{ id: "data-query", transport: { type: "stdio" }, auth: { type: "none", credentialConfigured: false }, tools: [] }], hooks: [] },
];
const skills = [
  { skillId: "plugin-banana-0:banana", name: "banana", description: "合成扩展说明", enabled: true, allowImplicitInvocation: true, allowedTools: ["read", "bash"] },
];
let preview = { role: "owner", agentSet: "multi" };
const notes = new Map([["note_1", {
  id: "note_1",
  displayName: "个人笔记",
  markdown: "# 欢迎来到 Centaeris\n\n这是你的第一份私人文档。你可以直接改写、重命名或删除它。",
  objectKind: "note",
  contentType: "text/markdown",
  status: "ready",
  sizeBytes: 55,
  updatedAt: "2026-08-01T00:00:00Z",
}]]);

function readJson(request) {
  return new Promise((resolve, reject) => {
    let body = "";
    request.setEncoding("utf8");
    request.on("data", (chunk) => { body += chunk; });
    request.on("end", () => {
      try { resolve(body ? JSON.parse(body) : {}); } catch (error) { reject(error); }
    });
    request.on("error", reject);
  });
}

function json(response, status, body) {
  response.writeHead(status, { "Content-Type": "application/json; charset=utf-8" });
  response.end(JSON.stringify(body));
}

const server = http.createServer(async (request, response) => {
  const url = new URL(request.url, "http://localhost:8000");
  response.setHeader("Access-Control-Allow-Origin", "http://localhost:3000");
  response.setHeader("Access-Control-Allow-Credentials", "true");
  response.setHeader("Access-Control-Allow-Headers", "Content-Type, X-CSRFToken");
  response.setHeader("Access-Control-Allow-Methods", "GET, POST, PUT, PATCH, DELETE, OPTIONS");

  if (request.method === "OPTIONS") return response.writeHead(204).end();
  if (url.pathname === "/__preview") {
    const role = url.searchParams.get("role");
    const agentSet = url.searchParams.get("agents");
    if (role && !["owner", "member"].includes(role)) return json(response, 400, { error: "banana_role" });
    if (agentSet && !["empty", "single", "multi"].includes(agentSet)) return json(response, 400, { error: "banana_agents" });
    preview = { role: role || preview.role, agentSet: agentSet || preview.agentSet };
    return json(response, 200, preview);
  }
  if (url.pathname === "/api/csrf") return json(response, 200, { csrfToken: "preview-csrf" });
  if (url.pathname === "/api/me") return json(response, 200, { user: { id: "user_1", email: preview.role === "owner" ? "owner@example.com" : "member@example.com", isStaff: false, isSuperuser: false } });
  if (url.pathname === "/api/workspaces") return json(response, 200, { workspaces: [{ id: "ws_1", name: "Default", status: "active", role: preview.role }] });
  if (url.pathname === "/api/workspaces/ws_1/agents") {
    const count = preview.agentSet === "empty" ? 0 : preview.agentSet === "single" ? 1 : agents.length;
    return json(response, 200, { agents: agents.slice(0, count) });
  }
  if (url.pathname === "/api/models") return json(response, 200, { models: [{ id: "model_1", displayName: "Preview Model", providerId: "preview", providerDisplayName: "预览", modelName: "preview-model" }] });
  if (url.pathname === "/api/workspaces/ws_1/session-projects") return json(response, 200, { projects: [] });
  if (url.pathname === "/api/workspaces/ws_1/sessions") return json(response, 200, { sessions: [] });
  if (url.pathname === "/api/workspaces/ws_1/members") return json(response, 200, { members: [
    { membershipId: "membership_owner", userId: "user_1", email: "owner@example.com", role: "owner", createdAt: "2026-08-01T00:00:00Z" },
    { membershipId: "membership_member", userId: "user_2", email: "member@example.com", role: "member", createdAt: "2026-08-02T00:00:00Z" },
  ] });
  if (url.pathname === "/api/workspaces/ws_1/invitations") return json(response, 200, { invitations: [] });
  if (url.pathname === "/api/workspaces/ws_1/groups") return json(response, 200, { groups: [] });
  if (url.pathname === "/api/workspaces/ws_1/plugins") return json(response, 200, { plugins });
  const pluginMatch = url.pathname.match(/^\/api\/workspaces\/ws_1\/plugins\/([^/]+)$/);
  if (pluginMatch && request.method === "PATCH") {
    const plugin = plugins.find((item) => item.name === decodeURIComponent(pluginMatch[1]));
    if (!plugin) return json(response, 404, { error: "plugin_not_found" });
    plugin.enabled = Boolean((await readJson(request)).enabled);
    return json(response, 200, { plugin });
  }
  if (url.pathname === "/api/workspaces/ws_1/skills") return json(response, 200, { schema: "workspace.skill.catalog.result.v1", skills });
  if (url.pathname === "/api/library" && request.method === "GET") return json(response, 200, { objects: [...notes.values()].map(({ markdown: _markdown, ...note }) => note) });
  if (url.pathname === "/api/library/notes" && request.method === "POST") {
    const body = await readJson(request);
    const id = `note_${notes.size + 1}`;
    const note = { id, displayName: body.displayName, markdown: body.markdown, objectKind: "note", contentType: "text/markdown", status: "ready", sizeBytes: body.markdown.length, updatedAt: new Date().toISOString() };
    notes.set(id, note);
    const { markdown: _markdown, ...object } = note;
    return json(response, 201, { object });
  }
  const noteMatch = url.pathname.match(/^\/api\/library\/(note_\d+)(\/note)?$/);
  if (noteMatch && notes.has(noteMatch[1])) {
    const note = notes.get(noteMatch[1]);
    if (noteMatch[2] && request.method === "PUT") Object.assign(note, await readJson(request), { updatedAt: new Date().toISOString() });
    const { markdown, ...object } = note;
    return json(response, 200, noteMatch[2] ? { object, markdown } : { object });
  }
  if (["/api/login", "/api/logout"].includes(url.pathname) && request.method === "POST") return response.writeHead(204).end();
  return json(response, 404, { error: "not_found" });
});

server.listen(8000, "127.0.0.1", () => console.log("Workspace Web preview API: http://localhost:8000"));
