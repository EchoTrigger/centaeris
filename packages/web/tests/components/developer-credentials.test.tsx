// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { afterEach, expect, test, vi } from "vitest";
import { clearCsrfToken, configureApi } from "../../src/api";
import { changeLanguage } from "../../src/i18n";
import SettingsRoute from "../../src/routes/SettingsRoute";

afterEach(() => {
  cleanup();
  clearCsrfToken();
  localStorage.removeItem("centaeris:language:v1");
  vi.unstubAllGlobals();
});

async function openDeveloper(role: "owner" | "member", query = "") {
  await changeLanguage("en");
  configureApi({ apiBaseUrl: "https://workspace.example" });
  const router = createMemoryRouter([{
    id: "authenticated",
    loader: () => ({ user: { id: "member", isSuperuser: role === "member" } }),
    HydrateFallback: () => null,
    children: [{
      id: "workspace",
      path: "w/:workspaceId",
      loader: ({ params }) => ({ workspace: { id: params.workspaceId, role } }),
      children: [{ path: "settings/developer", Component: SettingsRoute }],
    }],
  }], { initialEntries: ["/w/research/settings/developer" + query] });
  render(<RouterProvider router={router} />);
  return router;
}

test("owners can create scoped credentials, copy the secret after dismissing its notice, and confirm revocation", async () => {
  const user = userEvent.setup();
  const writeText = vi.spyOn(navigator.clipboard, "writeText");
  const secret = "credential_1.test_secret";
  const credential = {
    id: "credential_1", workspaceId: "research", name: "Document worker", modelConfigRef: "model_1",
    pluginNames: ["documents"], sourceObjectRefs: ["file_1"], createdAt: "2026-11-06T12:00:00Z", revokedAt: null,
  };
  const credentialPath = "/api/workspaces/research/api-credentials";
  const writes: Array<{ path: string; method: string; csrf: string | null; body: unknown }> = [];
  let hasCredential = false;
  vi.stubGlobal("fetch", async (url: string, options: RequestInit) => {
    const { pathname: path } = new URL(url);
    if (path === "/api/csrf") return Response.json({ csrfToken: "session-csrf" });
    if (options.method === "POST" || options.method === "DELETE") {
      expect(options.credentials).toBe("include");
      const csrf = new Headers(options.headers).get("X-CSRFToken");
      const body = typeof options.body === "string" ? JSON.parse(options.body) : undefined;
      writes.push({ path, method: options.method, csrf, body });
    }
    if (path === credentialPath && options.method === "POST") {
      hasCredential = true;
      return Response.json({ credential, secret }, { status: 201 });
    }
    if (path === credentialPath) return Response.json({ credentials: hasCredential ? [credential] : [] });
    if (path === credentialPath + "/credential_1" && options.method === "DELETE") {
      return Response.json({ credential: { ...credential, revokedAt: "2026-11-06T12:03:00Z" } });
    }
    if (path === "/api/models") return Response.json({ models: [{ id: "model_1", displayName: "Document model" }] });
    if (path.endsWith("/plugins")) return Response.json({ plugins: [
      { name: "documents", displayName: "Documents", enabled: true },
      { name: "disabled", displayName: "Disabled plugin", enabled: false },
    ] });
    if (path.endsWith("/sources")) return Response.json({ sources: [{ id: "source_1", name: "Handbook", status: "ready" }] });
    if (path.endsWith("/sources/source_1/objects")) return Response.json({ objects: [
      { id: "file_1", displayPath: "policy.docx", status: "ready", objectType: "file" },
      { id: "folder_1", displayPath: "Archive", status: "ready", objectType: "folder" },
    ] });
    throw new Error("Unexpected URL: " + url);
  });
  await openDeveloper("owner");

  expect(await screen.findByText("https://workspace.example/api/v1/agent-runs", { exact: true })).toBeTruthy();
  await user.click(await screen.findByRole("button", { name: "Create credential" }));
  await user.type(screen.getByRole("textbox", { name: "Credential name" }), credential.name);
  await screen.findByRole("option", { name: "Document model" });
  await user.selectOptions(screen.getByRole("combobox", { name: "Model" }), credential.modelConfigRef);
  await user.click(screen.getByText("Optional permissions"));
  expect(screen.queryByRole("checkbox", { name: "Disabled plugin" })).toBeNull();
  await user.click(await screen.findByRole("checkbox", { name: "Documents" }));
  await screen.findByRole("option", { name: "Handbook" });
  await user.selectOptions(screen.getByRole("combobox", { name: "Material source" }), "source_1");
  await user.click(await screen.findByRole("checkbox", { name: "policy.docx" }));
  expect(screen.queryByRole("checkbox", { name: "Archive" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Create credential" }));

  expect(await screen.findByDisplayValue(secret)).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Copy secret" }));
  expect(writeText).toHaveBeenCalledWith(secret);
  await user.click(screen.getByRole("button", { name: "Done" }));
  expect(screen.queryByDisplayValue(secret)).toBeNull();
  writeText.mockClear();
  await user.click(screen.getByRole("button", { name: "Copy secret for Document worker" }));
  expect(writeText).toHaveBeenCalledWith(secret);
  await user.click(screen.getByText("Granted permissions"));
  expect(screen.getByText("file_1")).toBeTruthy();
  await user.click(screen.getByRole("link", { name: "Docs" }));
  await user.click(await screen.findByRole("link", { name: "Back to settings" }));
  await user.click(await screen.findByRole("button", { name: "Revoke Document worker" }));
  expect(screen.queryByDisplayValue(secret)).toBeNull();
  expect(writes).toHaveLength(1);
  await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Revoke" }));
  expect(await screen.findByText("Revoked")).toBeTruthy();
  expect(writes).toEqual([
    { path: credentialPath, method: "POST", csrf: "session-csrf", body: {
      name: credential.name, modelConfigRef: credential.modelConfigRef, pluginNames: credential.pluginNames,
      sourceObjectRefs: credential.sourceObjectRefs,
    } },
    { path: credentialPath + "/" + credential.id, method: "DELETE", csrf: "session-csrf", body: undefined },
  ]);
});

test("a basic credential loads only models and can be created without optional catalogs", async () => {
  const user = userEvent.setup();
  const requests: string[] = [];
  const writes: unknown[] = [];
  const credential = {
    id: "basic", name: "Basic worker", workspaceId: "research", modelConfigRef: "model_1",
    pluginNames: [], sourceObjectRefs: [], revokedAt: null,
  };
  vi.stubGlobal("fetch", async (url: string, options: RequestInit) => {
    const { pathname: path } = new URL(url);
    requests.push(path);
    if (path === "/api/csrf") return Response.json({ csrfToken: "session-csrf" });
    if (path.endsWith("/api-credentials")) {
      if (options.method === "POST") {
        writes.push(JSON.parse(String(options.body)));
        return Response.json({ credential, secret: "basic.one_time_secret" }, { status: 201 });
      }
      return Response.json({ credentials: [] });
    }
    if (path === "/api/models") return Response.json({ models: [{ id: "model_1", displayName: "Basic model" }] });
    // An unavailable optional service must not delay the basic form.
    if (path.endsWith("/plugins") || path.endsWith("/sources")) return new Promise<Response>(() => {});
    throw new Error("Unexpected URL: " + url);
  });
  await openDeveloper("owner");
  await user.click(await screen.findByRole("button", { name: "Create credential" }));
  await user.type(screen.getByRole("textbox", { name: "Credential name" }), credential.name);
  await screen.findByRole("option", { name: "Basic model" });
  await user.selectOptions(screen.getByRole("combobox", { name: "Model" }), credential.modelConfigRef);
  expect(requests).not.toContain("/api/workspaces/research/plugins");
  expect(requests).not.toContain("/api/workspaces/research/sources");
  expect(screen.queryByRole("alert")).toBeNull();
  await user.click(screen.getByRole("button", { name: "Create credential" }));
  expect(await screen.findByDisplayValue("basic.one_time_secret")).toBeTruthy();
  expect(writes).toEqual([{
    name: credential.name, modelConfigRef: credential.modelConfigRef, pluginNames: [], sourceObjectRefs: [],
  }]);
});

test("optional catalogs fail and retry independently without clearing selections", async () => {
  const user = userEvent.setup();
  const requests: string[] = [];
  let finishSources: (response: Response) => void = () => {};
  const pendingSources = new Promise<Response>(resolve => { finishSources = resolve; });
  let pluginAttempts = 0;
  let sourceAttempts = 0;
  vi.stubGlobal("fetch", async (url: string) => {
    const { pathname: path } = new URL(url);
    requests.push(path);
    if (path.endsWith("/api-credentials")) return Response.json({ credentials: [] });
    if (path === "/api/models") return Response.json({ models: [{ id: "model_1", displayName: "Basic model" }] });
    if (path.endsWith("/plugins")) return ++pluginAttempts === 1
      ? Response.json({ error: "temporarily_unavailable" }, { status: 503 })
      : Response.json({ plugins: [{ name: "documents", displayName: "Documents", enabled: true }] });
    if (path.endsWith("/sources")) return ++sourceAttempts === 1 ? pendingSources
      : Response.json({ sources: [{ id: "source_1", name: "Handbook", status: "ready" }] });
    if (path.endsWith("/sources/source_1/objects")) return Response.json({ objects: [
      { id: "file_1", displayPath: "policy.docx", status: "ready", objectType: "file" },
    ] });
    throw new Error("Unexpected URL: " + url);
  });
  await openDeveloper("owner");
  await user.click(await screen.findByRole("button", { name: "Create credential" }));
  await user.type(screen.getByRole("textbox", { name: "Credential name" }), "Scoped worker");
  await screen.findByRole("option", { name: "Basic model" });
  await user.selectOptions(screen.getByRole("combobox", { name: "Model" }), "model_1");
  await user.click(screen.getByText("Optional permissions"));

  const plugins = within(await screen.findByRole("group", { name: "Plugins" }));
  const files = within(screen.getByRole("group", { name: "Files" }));
  expect(await plugins.findByRole("alert")).toHaveProperty("textContent", expect.stringContaining("Could not load workspace plugins."));
  expect(plugins.queryByText("No enabled plugins to select.")).toBeNull();
  expect(files.getByRole("status")).toBeTruthy();
  expect((screen.getByRole("combobox", { name: "Model" }) as HTMLSelectElement).value).toBe("model_1");
  expect((screen.getByRole("button", { name: "Create credential" }) as HTMLButtonElement).disabled).toBe(false);

  await user.click(plugins.getByRole("button", { name: "Retry" }));
  await user.click(await plugins.findByRole("checkbox", { name: "Documents" }));
  await act(async () => { finishSources(Response.json({ error: "temporarily_unavailable" }, { status: 503 })); });
  expect(await files.findByRole("alert")).toHaveProperty("textContent", expect.stringContaining("Could not load material sources."));
  expect(files.queryByText("No available material sources.")).toBeNull();
  await user.click(files.getByRole("button", { name: "Retry" }));
  await files.findByRole("option", { name: "Handbook" });
  await user.selectOptions(files.getByRole("combobox", { name: "Material source" }), "source_1");
  await user.click(await files.findByRole("checkbox", { name: "policy.docx" }));
  await user.click(screen.getByText("Optional permissions"));
  await user.click(screen.getByText("Optional permissions"));
  expect((await screen.findByRole("checkbox", { name: "Documents" }) as HTMLInputElement).checked).toBe(true);
  expect((screen.getByRole("checkbox", { name: "policy.docx" }) as HTMLInputElement).checked).toBe(true);
  expect((screen.getByRole("combobox", { name: "Model" }) as HTMLSelectElement).value).toBe("model_1");
  expect(screen.getByDisplayValue("Scoped worker")).toBeTruthy();
  expect(requests.filter(path => path === "/api/models")).toHaveLength(1);
  expect(pluginAttempts).toBe(2);
  expect(sourceAttempts).toBe(2);
  expect(requests.filter(path => path.endsWith("/objects"))).toHaveLength(1);
});

test("switching workspace aborts pending optional catalogs and resets credential form state", async () => {
  const user = userEvent.setup();
  const oldSignals: AbortSignal[] = [];
  let finishPlugins: (response: Response) => void = () => {};
  let finishSources: (response: Response) => void = () => {};
  const pendingPlugins = new Promise<Response>(resolve => { finishPlugins = resolve; });
  const pendingSources = new Promise<Response>(resolve => { finishSources = resolve; });
  vi.stubGlobal("fetch", async (url: string, options: RequestInit) => {
    const { pathname: path, searchParams } = new URL(url);
    if (path.endsWith("/api-credentials")) return Response.json({ credentials: [] });
    if (path === "/api/models") {
      const workspace = searchParams.get("workspaceId");
      return Response.json({ models: [{ id: workspace + "_model", displayName: workspace + " model" }] });
    }
    if (path === "/api/workspaces/research/plugins") {
      oldSignals.push(options.signal as AbortSignal);
      return pendingPlugins;
    }
    if (path === "/api/workspaces/research/sources") {
      oldSignals.push(options.signal as AbortSignal);
      return pendingSources;
    }
    throw new Error("Unexpected URL: " + url);
  });
  const router = await openDeveloper("owner");
  await user.click(await screen.findByRole("button", { name: "Create credential" }));
  await user.type(screen.getByRole("textbox", { name: "Credential name" }), "Research worker");
  await screen.findByRole("option", { name: "research model" });
  await user.selectOptions(screen.getByRole("combobox", { name: "Model" }), "research_model");
  await user.click(screen.getByText("Optional permissions"));
  await waitFor(() => expect(oldSignals).toHaveLength(2));

  await act(async () => { await router.navigate("/w/engineering/settings/developer"); });
  expect(oldSignals.every(signal => signal.aborted)).toBe(true);
  expect(screen.queryByDisplayValue("Research worker")).toBeNull();
  await user.click(await screen.findByRole("button", { name: "Create credential" }));
  await screen.findByRole("option", { name: "engineering model" });
  await act(async () => {
    finishPlugins(Response.json({ plugins: [{ name: "old", displayName: "Old plugin", enabled: true }] }));
    finishSources(Response.json({ sources: [{ id: "old", name: "Old source", status: "ready" }] }));
  });
  expect((screen.getByRole("textbox", { name: "Credential name" }) as HTMLInputElement).value).toBe("");
  expect(screen.queryByRole("option", { name: "research model" })).toBeNull();
  expect(screen.queryByText("Old plugin")).toBeNull();
  expect(screen.queryByText("Old source")).toBeNull();
  expect(screen.queryByRole("alert")).toBeNull();
});

test("a workspace member can open a direct docs link without loading credential management", async () => {
  const fetch = vi.fn();
  vi.stubGlobal("fetch", fetch);
  await openDeveloper("member", "?view=docs&topic=artifacts");
  expect(await screen.findByRole("heading", { name: "Artifacts", level: 1 })).toBeTruthy();
  await userEvent.click(screen.getByRole("link", { name: "Back to settings" }));
  expect(await screen.findByRole("heading", { name: "One-time Agent API" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Create credential" })).toBeNull();
  expect(fetch).not.toHaveBeenCalled();
});
