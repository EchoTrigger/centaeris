// @vitest-environment jsdom
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import WorkspaceProfileSettings from "../../src/routes/WorkspaceProfileSettings";
import { apiUrl, configureApi, requireWorkspaces } from "../../src/api";
import { useWorkspaceDocumentIdentity, workspaceInitialIcon } from "../../src/shell/useWorkspaceDocumentIdentity";
import { WORKSPACE_IDENTITY_CACHE_KEY, clearWorkspaceIdentityCache } from "../../src/shell/workspaceIdentityCache";

const fixture = vi.hoisted(() => ({ request: vi.fn(), revalidate: vi.fn() }));
vi.mock("../../src/api", async original => ({ ...await original<object>(), apiJson: (...args: unknown[]) => fixture.request(...args) }));
vi.mock("react-router", async original => ({ ...await original<object>(), useRevalidator: () => ({ revalidate: fixture.revalidate }) }));
vi.mock("../../src/i18n", () => ({ useTranslation: () => ({ t: (key: string, options?: { setting: string }) => options?.setting ? `${key} ${options.setting}` : key }) }));
const workspace = { id: "workspace", name: "Research", role: "owner", avatarUrl: null };
const customAvatar = `/api/workspaces/workspace/avatar/${"a".repeat(64)}`;
beforeEach(() => {
  configureApi({ apiBaseUrl: window.location.origin });
  fixture.request.mockReset(); fixture.revalidate.mockReset();
});
afterEach(() => { cleanup(); sessionStorage.clear(); vi.restoreAllMocks(); });

test("editing a workspace name keeps a rejected draft and discards cancelled changes", async () => {
  const user = userEvent.setup();
  render(<WorkspaceProfileSettings workspace={workspace} />);
  const edit = screen.getByRole("button", { name: "settingsRoute.editSetting workspaceProfile.name" });
  expect(screen.queryByRole("textbox")).toBeNull();
  await user.click(edit);
  const dialog = within(screen.getByRole("dialog", { name: "workspaceProfile.name" }));
  const save = dialog.getByRole("button", { name: "modelSettings.save" }) as HTMLButtonElement;
  expect(save.disabled).toBe(true);
  const name = dialog.getByRole("textbox", { name: "workspaceProfile.name" }) as HTMLInputElement;
  await user.clear(name); await user.type(name, "New research");
  fixture.request.mockRejectedValueOnce(new Error("offline"));
  await user.click(save);
  await dialog.findByRole("alert");
  expect(name.value).toBe("New research");
  expect(save.disabled).toBe(false);
  expect(fixture.revalidate).not.toHaveBeenCalled();
  fixture.request.mockResolvedValueOnce({ workspace: { ...workspace, name: "New research" } });
  await user.click(save);
  await screen.findByRole("status");
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.getByText("New research")).toBeTruthy();
  expect(JSON.parse(fixture.request.mock.calls[1][1].body)).toEqual({ name: "New research" });
  expect(fixture.request.mock.calls[1][0]).toBe("/api/workspaces/workspace/profile");
  expect(fixture.revalidate).toHaveBeenCalledTimes(1);
  await user.click(edit);
  await user.type(screen.getByRole("textbox"), " draft");
  await user.click(screen.getByRole("button", { name: "agentRunRow.cancel" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  await user.click(edit);
  expect(screen.getByDisplayValue("New research")).toBeTruthy();
  expect(fixture.request).toHaveBeenCalledTimes(2);
});

test("avatar editor submits removal and replacement together with the workspace name", async () => {
  const user = userEvent.setup();
  render(<WorkspaceProfileSettings workspace={{ ...workspace, avatarUrl: customAvatar }} />);
  const edit = screen.getByRole("button", { name: "settingsRoute.editSetting workspaceProfile.avatar" });
  await user.click(edit);
  fixture.request.mockResolvedValueOnce({ workspace });
  await user.click(screen.getByRole("button", { name: "workspaceProfile.remove" }));
  await user.click(screen.getByRole("button", { name: "modelSettings.save" }));
  await screen.findByRole("status");
  expect(JSON.parse(fixture.request.mock.calls[0][1].body)).toEqual({ name: "Research", avatarDataUrl: null });
  await user.click(edit);
  const input = screen.getByLabelText("workspaceProfile.upload", { selector: "input" }) as HTMLInputElement;
  await user.upload(input, new File(["png"], "avatar.png", { type: "image/png" }));
  const save = screen.getByRole("button", { name: "modelSettings.save" }) as HTMLButtonElement;
  await waitFor(() => expect(save.disabled).toBe(false));
  fixture.request.mockResolvedValueOnce({ workspace: { ...workspace, avatarUrl: customAvatar } });
  await user.click(save);
  await screen.findByRole("status");
  expect(JSON.parse(fixture.request.mock.calls[1][1].body)).toEqual({ name: "Research", avatarDataUrl: "data:image/png;base64,cG5n" });
});

test("invalid avatar format and size leave the existing avatar unchanged", async () => {
  const user = userEvent.setup({ applyAccept: false });
  render(<WorkspaceProfileSettings workspace={{ ...workspace, avatarUrl: customAvatar }} />);
  await user.click(screen.getByRole("button", { name: "settingsRoute.editSetting workspaceProfile.avatar" }));
  const dialog = screen.getByRole("dialog", { name: "workspaceProfile.avatar" });
  const input = within(dialog).getByLabelText("workspaceProfile.upload", { selector: "input" }) as HTMLInputElement;
  for (const file of [new File(["gif"], "avatar.gif", { type: "image/gif" }), new File([new Uint8Array(1024 * 1024 + 1)], "avatar.png", { type: "image/png" })]) {
    await user.upload(input, file);
    expect(within(dialog).getByRole("alert").textContent).toBe("workspaceProfile.avatarInvalid");
    expect(dialog.querySelector("img")?.getAttribute("src")).toBe(apiUrl(customAvatar));
    expect((within(dialog).getByRole("button", { name: "modelSettings.save" }) as HTMLButtonElement).disabled).toBe(true);
  }
  expect(fixture.request).not.toHaveBeenCalled();
});

test("workspace identity follows saved name and avatar and restores every original favicon", () => {
  const title = document.title;
  const originalHead = document.head.innerHTML;
  document.title = "Platform";
  document.head.insertAdjacentHTML("beforeend", '<link rel="icon" href="/one.png"><link rel="icon" href="/two.png"><link rel="apple-touch-icon" href="/touch.png">');
  const originalIcons = Array.from(document.querySelectorAll('link[rel="icon"]'));
  function Identity({ name, avatarUrl }: { name: string; avatarUrl: string | null }) {
    useWorkspaceDocumentIdentity({ id: "workspace", name, avatarUrl });
    return null;
  }
  const view = render(<Identity name="<&" avatarUrl={null} />);
  expect(JSON.parse(sessionStorage.getItem(WORKSPACE_IDENTITY_CACHE_KEY)!)).toEqual({
    schema: "workspace.identity.v1", workspaceId: "workspace", name: "<&", avatarUrl: null,
  });
  expect(document.title).toBe("<&");
  const svg = decodeURIComponent(workspaceInitialIcon("<&").split(",")[1]);
  expect(new DOMParser().parseFromString(svg, "image/svg+xml").querySelector("text")?.textContent).toBe("<");
  expect(document.querySelectorAll('link[rel="icon"]')).toHaveLength(1);
  view.rerender(<Identity name="Renamed" avatarUrl={customAvatar} />);
  expect(document.title).toBe("Renamed");
  expect(document.querySelector('link[rel="icon"]')?.getAttribute("href")).toBe(apiUrl(customAvatar));
  view.rerender(<Identity name="Renamed" avatarUrl={null} />);
  expect(document.querySelector('link[rel="icon"]')?.getAttribute("href")).toBe(workspaceInitialIcon("Renamed"));
  view.unmount();
  expect(document.title).toBe("Platform");
  expect(Array.from(document.querySelectorAll('link[rel="icon"]'))).toEqual(originalIcons);
  expect(document.querySelector('link[rel="apple-touch-icon"]')?.getAttribute("href")).toBe("/touch.png");
  document.head.innerHTML = originalHead;
  document.title = title;
});

test("leaving a bootstrapped Workspace restores the platform identity and logout clears the cache", () => {
  const title = document.title;
  const head = document.head.innerHTML;
  document.title = "Cached workspace";
  document.head.insertAdjacentHTML("beforeend", `<link rel="icon" href="${customAvatar}" data-workspace-identity>`);
  function Identity() { useWorkspaceDocumentIdentity({ id: "workspace", name: "Saved workspace", avatarUrl: customAvatar }); return null; }
  const view = render(<Identity />);
  expect(document.title).toBe("Saved workspace");
  view.unmount();
  expect(document.title).toBe("Centaeris");
  expect(document.head.querySelector('link[rel="icon"]')?.getAttribute("href")).toBe("/centaeris-mark.png");
  expect(sessionStorage.getItem(WORKSPACE_IDENTITY_CACHE_KEY)).not.toBeNull();
  clearWorkspaceIdentityCache();
  expect(sessionStorage.getItem(WORKSPACE_IDENTITY_CACHE_KEY)).toBeNull();
  document.head.innerHTML = head;
  document.title = title;
});

test("workspace summaries require an explicitly null or same-workspace content-addressed avatar", () => {
  expect(requireWorkspaces([workspace])).toEqual([workspace]);
  expect(requireWorkspaces([{ ...workspace, avatarUrl: customAvatar }])[0].avatarUrl).toBe(customAvatar);
  for (const avatarUrl of [undefined, "https://example.test/avatar.png", "/api/workspaces/other/avatar/" + "a".repeat(64)]) {
    expect(() => requireWorkspaces([{ ...workspace, avatarUrl }])).toThrow("workspaces_invalid");
  }
});
