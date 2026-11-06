// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
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
      loader: () => ({ workspace: { id: "research", role } }),
      children: [{ path: "settings/developer", Component: SettingsRoute }],
    }],
  }], { initialEntries: ["/w/research/settings/developer" + query] });
  render(<RouterProvider router={router} />);
}

test("owners can create scoped credentials, copy the one-time secret, and confirm revocation", async () => {
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
  await user.click(screen.getByRole("checkbox", { name: "Documents" }));
  await user.selectOptions(screen.getByRole("combobox", { name: "Material source" }), "source_1");
  await user.click(await screen.findByRole("checkbox", { name: "policy.docx" }));
  expect(screen.queryByRole("checkbox", { name: "Archive" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Create credential" }));

  expect(await screen.findByDisplayValue(secret)).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Copy secret" }));
  expect(writeText).toHaveBeenCalledWith(secret);
  await user.click(screen.getByRole("button", { name: "Done" }));
  expect(screen.queryByDisplayValue(secret)).toBeNull();
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
