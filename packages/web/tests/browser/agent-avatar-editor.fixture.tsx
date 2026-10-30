import { createRoot } from "react-dom/client";
import { useState } from "react";
import { apiJson, configureApi, jsonOptions } from "../../src/api";
import { i18n } from "../../src/i18n";
import { AgentEditorModal } from "../../src/shell/AgentEditorModal";
import { AgentMark } from "../../src/shell/AgentMark";
import { AgentDocumentIdentity } from "../../src/shell/useAgentDocumentIdentity";
import "../../src/globals.css";

configureApi({ apiBaseUrl: location.origin });
await i18n.changeLanguage("en");
document.documentElement.dataset.theme = new URLSearchParams(location.search).get("theme") ?? "light";
const requests: { path: string; method: string; body?: Record<string, unknown> }[] = [];
const failures: string[] = [];
window.addEventListener("error", event => failures.push(event.message));
window.addEventListener("unhandledrejection", event => failures.push(String(event.reason)));
let stored = { id: "fixture-agent", name: "Private Agent", description: "Synthetic avatar editor", instructions: "Keep the existing instructions.", avatarKind: "centaeris", avatarUrl: null as string | null };
window.fetch = async (input, init) => {
  const path = new URL(String(input), location.origin).pathname;
  const method = init?.method ?? "GET";
  const body = typeof init?.body === "string" ? JSON.parse(init.body) : undefined;
  requests.push({ path, method, body });
  const json = (value: unknown) => new Response(JSON.stringify(value), { status: 200, headers: { "Content-Type": "application/json" } });
  if (path === "/api/csrf" && method === "GET") return json({ csrfToken: "synthetic-csrf" });
  if (path === "/api/agents/fixture-agent" && method === "PATCH") {
    stored = { ...stored, name: body.name, description: body.description, instructions: body.instructions, avatarKind: body.avatarKind,
      avatarUrl: Object.hasOwn(body, "avatarDataBase64") ? body.avatarDataBase64 === null ? null : "/api/agents/fixture-agent/avatar?v=saved" : stored.avatarUrl };
    return json({ agent: stored });
  }
  throw new Error(`Unexpected avatar editor fixture request: ${method} ${path}`);
};
function Fixture() {
  const [agent, setAgent] = useState(stored);
  const [editing, setEditing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  async function save(profile: unknown) {
    setBusy(true); setError("");
    try {
      const result = await apiJson<{ agent: typeof stored }>("/api/agents/fixture-agent", jsonOptions("PATCH", profile));
      setAgent(result.agent); setEditing(false);
    } catch { setError("Synthetic profile save failed"); }
    finally { setBusy(false); }
  }
  return <><AgentDocumentIdentity agent={agent} /><main>
    <header data-saved-agent><AgentMark className="shWorkspaceAgentMark" agent={agent} /><h1>{agent.name}</h1></header>
    <button type="button" onClick={() => setEditing(true)}>Edit profile</button>
  </main>{editing ? <AgentEditorModal agent={agent} heading="Edit Agent" submitLabel="Save profile" onSave={save} onClose={() => setEditing(false)} busy={busy} error={error} /> : null}</>;
}
createRoot(document.getElementById("fixture")!).render(<Fixture />);
Object.assign(window, { avatarEditorFixture: { requests, failures } });
