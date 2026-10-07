import { createRoot } from "react-dom/client";
import { createMemoryRouter, RouterProvider } from "react-router";
import { configureApi } from "../../src/api";
import { i18n, t } from "../../src/i18n";
import SettingsRoute from "../../src/routes/SettingsRoute";

// Synthetic frontend/API behavior only. This does not certify model connectivity,
// input delivery, coordinator admission or the unrun backend database tests.
const workspace = { id: "fixture-workspace", name: "Synthetic workspace", role: "member" };
const agent = { id: "fixture-agent", workspaceId: workspace.id, name: "Synthetic Agent", description: "", instructions: "", avatarKind: "centaeris", status: "active", deletedAt: null, createdAt: "2026-09-30T14:00:00Z", updatedAt: "2026-09-30T14:00:00Z" };
const user = { id: "fixture-user", email: "fixture@example.invalid", isStaff: false, isSuperuser: false };
const model = { id: "fixture-exact-model", displayName: "Exact synthetic model", providerId: "fixture-provider", providerDisplayName: "Synthetic provider", modelName: "synthetic-native", contextTokens: 1000, maxOutputTokens: 100, thinkingMode: "high", thinkingModes: ["low", "high"] };
let saved: { schema: string; agentId: string; modelConfigRef: string | null; thinkingMode: string | null; status: string } = { schema: "agent.model_settings.v1", agentId: agent.id, modelConfigRef: null, thinkingMode: null, status: "unconfigured" };
const requests: { path: string; method: string; body?: unknown }[] = [];
configureApi({ apiBaseUrl: location.origin } as Parameters<typeof configureApi>[0]);
window.fetch = async (input, init) => {
  const path = new URL(String(input), location.origin).pathname; const method = init?.method || "GET";
  const body = typeof init?.body === "string" ? JSON.parse(init.body) : undefined;
  requests.push({ path, method, body });
  const json = (value: unknown) => Promise.resolve(new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } }));
  if (path === "/api/csrf" && method === "GET") return json({ csrfToken: "synthetic-csrf" });
  if (path === "/api/models" && method === "GET") return json({ models: [model] });
  if (path === `/api/agents/${agent.id}/model-settings`) {
    if (method === "PATCH") {
      if (Object.keys(body).sort().join("|") !== "modelConfigRef|schema|thinkingMode" || body.schema !== "agent.model_settings.update.v1") throw new Error("Unexpected synthetic settings request");
      saved = { ...saved, modelConfigRef: body.modelConfigRef, thinkingMode: body.modelConfigRef === null ? null : body.thinkingMode ?? "high", status: body.modelConfigRef === null ? "unconfigured" : "configured" };
    } else if (method !== "GET") throw new Error("Unexpected Settings method");
    return json(saved);
  }
  throw new Error(`Unexpected Settings fixture request: ${method} ${path}`);
};
const base = `/w/${workspace.id}/settings/agents`;
const selected = `${base}?agentId=${agent.id}`;
const router = createMemoryRouter([{ id: "authenticated", loader: () => ({ user }), children: [{ id: "workspace", path: "w/:workspaceId", loader: () => ({ workspace, workspaces: [workspace], agents: [agent] }), children: [{ path: "settings/agents", Component: SettingsRoute }, { path: "app", element: <p>Ordinary workspace route</p> }] }] }], { initialEntries: [selected] });
await i18n.changeLanguage("en");
document.documentElement.dataset.theme = new URLSearchParams(location.search).get("theme") || "light";
createRoot(document.getElementById("fixture")!).render(<RouterProvider router={router} />);
const results: { name: string; actual: unknown; expected: unknown }[] = [];
const check = (name: string, actual: unknown, expected: unknown) => results.push({ name, actual, expected });
const button = (label: string) => [...document.querySelectorAll<HTMLButtonElement>("button")].find(item => item.getAttribute("aria-label") === label || item.textContent?.trim() === label)!;
async function waitFor(read: () => boolean) { for (let attempt = 0; attempt < 100; attempt++) { if (read()) return; await new Promise(resolve => setTimeout(resolve, 30)); } throw new Error("Settings fixture did not reach expected state"); }
const writes = () => requests.filter(item => item.method === "PATCH");
try {
  await waitFor(() => Boolean(document.querySelector(".agentSettingsModelControls")));
  check("regular member sees private Agent Settings", Boolean(document.querySelector('.workspaceSettingsNav a[aria-current="page"]')), true);
  check("global admin model catalog is not used", requests.some(item => item.path.startsWith("/api/admin/")), false);
  check("unconfigured settings never select first model", document.body.textContent!.includes(t("agentSettings.unconfigured")), true);
  check("loading settings makes no mutation", writes().length, 0);
  document.querySelector<HTMLElement>(`summary[aria-label="${t("appRoute.aiModel")}"]`)!.click();
  button(model.displayName).click();
  await waitFor(() => !button(t("agentRoute.saveChanges"))?.disabled);
  button(t("agentRoute.saveChanges")).click();
  await waitFor(() => document.body.textContent!.includes(t("agentSettings.saved")));
  check("save sends null for server default resolution", writes()[0].body, { schema: "agent.model_settings.update.v1", modelConfigRef: model.id, thinkingMode: null });
  check("effective server effort is displayed", document.body.textContent!.includes(t("appRoute.high")), true);
  document.querySelector<HTMLElement>(`summary[aria-label="${t("appRoute.reasoningEffort")}"]`)!.click();
  button(t("appRoute.low")).click();
  await waitFor(() => !button(t("agentRoute.saveChanges"))?.disabled);
  button(t("agentRoute.saveChanges")).click();
  await waitFor(() => writes().length === 2 && document.body.textContent!.includes(t("agentSettings.saved")));
  check("explicit effort is persisted", (writes()[1].body as { thinkingMode: string }).thinkingMode, "low");
  button(t("agentSettings.clearModel")).click();
  await waitFor(() => !button(t("agentRoute.saveChanges"))?.disabled);
  button(t("agentRoute.saveChanges")).click();
  await waitFor(() => writes().length === 3 && document.body.textContent!.includes(t("agentSettings.unconfigured")));
  check("clear has exact null fields", writes()[2].body, { schema: "agent.model_settings.update.v1", modelConfigRef: null, thinkingMode: null });
  saved = { ...saved, modelConfigRef: "fixture-unavailable-model", thinkingMode: "high", status: "unavailable" };
  await router.navigate(base);
  await waitFor(() => !document.querySelector(".agentSettingsModelControls"));
  await router.navigate(selected);
  await waitFor(() => document.body.textContent!.includes(t("agentSettings.unavailable")));
  check("unavailable saved selection is not replaced", writes().length, 3);
  button(t("agentRoute.editAgent")).click();
  await waitFor(() => Boolean(document.querySelector('.agentSettingsEditorLayer [role="dialog"]')));
  const layer = document.querySelector<HTMLElement>(".agentSettingsEditorLayer")!;
  const settingsPage = document.querySelector<HTMLElement>(".settingsModalPage")!;
  const profile = layer.querySelector<HTMLElement>('[role="dialog"]')!;
  const bounds = profile.getBoundingClientRect();
  check("Agent editor layer is above Settings", Number(getComputedStyle(layer).zIndex) > Number(getComputedStyle(settingsPage).zIndex), true);
  check("Agent editor receives pointer hits above Settings", layer.contains(document.elementFromPoint(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2)), true);
  button(t("agentEditorModal.editSoulMd")).click();
  await waitFor(() => Boolean(layer.querySelector("textarea")) && Boolean(layer.querySelector('[role="dialog"]')?.contains(document.activeElement)));
  check("SOUL retains a modal focus boundary", layer.querySelector('[role="dialog"]')?.getAttribute("aria-modal"), "true");
  check("Settings stays inert during SOUL editing", Boolean(settingsPage.closest("[inert]")), true);
  layer.querySelector<HTMLElement>('[role="dialog"]')!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
  await waitFor(() => Boolean(layer.querySelector("form")) && Boolean(layer.querySelector("form")?.contains(document.activeElement)));
  layer.querySelector<HTMLButtonElement>(`button[aria-label="${t("workspaceContextPanel.close")}"]`)!.click();
  await waitFor(() => !document.querySelector(".agentSettingsEditorLayer"));
  button(t("agentCreateRoute.createAgent")).click();
  await waitFor(() => Boolean(document.querySelector(`[role="dialog"][aria-label="${t("agentCreateRoute.createPrivateAgent")}"]`)));
  check("Agent creation opens from Settings", new URLSearchParams(router.state.location.search).get("new"), "1");
  check("no input submission or Run endpoint is called", requests.every(item => item.method === "GET" || item.path.endsWith("/model-settings")), true);
  check("all Agent Settings checks completed", true, true);
} catch (error) { check("Settings fixture completed", error instanceof Error ? error.message : String(error), "success"); }
finally { document.getElementById("results")!.textContent = JSON.stringify(results); }
