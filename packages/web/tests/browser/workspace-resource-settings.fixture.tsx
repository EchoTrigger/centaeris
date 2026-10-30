import { createRoot } from "react-dom/client";
import OperatorWorkspaceResources from "../../src/routes/OperatorWorkspaceResources";
import { configureApi } from "../../src/api";
import { i18n } from "../../src/i18n";
import "../../src/globals.css";

const parameters = new URLSearchParams(location.search);
document.documentElement.dataset.theme = parameters.get("theme") ?? "light";
await i18n.changeLanguage("en");
configureApi({ apiBaseUrl: location.origin });
const failures: string[] = [];
window.addEventListener("error", event => failures.push(event.message));
window.addEventListener("unhandledrejection", event => failures.push(String(event.reason)));
const plugins = Array.from({ length: 24 }, (_, index) => ({
  name: `synthetic-${index}`, displayName: `Synthetic plugin ${index + 1}`, packageDigest: `sha256:${index}`,
  shortDescription: "Workspace plugin with a documented skill and no additional connection configuration.",
  version: "1.0.0", enabled: true, errors: [], capabilities: ["skills"], hooks: [], mcpServers: [], scope: "workspace", writable: true,
}));
window.fetch = async (input) => {
  const path = new URL(String(input), location.origin).pathname;
  let data;
  if (path === "/api/admin/workspace-resources") data = { workspaces: [{ id: "synthetic", name: "Synthetic workspace", status: "active" }] };
  else if (path.endsWith("/plugins")) data = { plugins };
  else if (path.includes("/plugins/")) data = { plugin: plugins.find(plugin => path.endsWith("/" + plugin.name)) };
  else if (path.endsWith("/model-providers")) data = { providers: [] };
  else if (path.endsWith("/models")) data = { models: [] };
  else if (path.endsWith("/model-provider-templates")) data = { templates: [] };
  else if (path.endsWith("/mcp-bearer-credentials")) data = { credentials: [] };
  else throw new Error(`Unexpected synthetic request: ${path}`);
  return new Response(JSON.stringify(data), { status: 200, headers: { "Content-Type": "application/json" } });
};
createRoot(document.getElementById("fixture")!).render(
  <main className="settingsModalPage">
    <section className="settingsDialog workspaceSettingsDialog" role="dialog" aria-label="Synthetic resource settings">
      {parameters.get("contentOnly") ? null : <aside className="workspaceSettingsNav"><h2>Workspace settings</h2></aside>}
      <div className="workspaceSettingsContent"><div className="workspaceSettingsScroll"><OperatorWorkspaceResources isSuperuser /></div></div>
    </section>
  </main>,
);
Object.assign(window, { workspaceResourceSettingsFixture: { failures } });
