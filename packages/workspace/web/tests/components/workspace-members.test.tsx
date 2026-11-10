// @vitest-environment jsdom
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { MemoryRouter } from "react-router";
import WorkspaceMembersRoute from "../../src/routes/WorkspaceMembersRoute";

const fixture = vi.hoisted(() => ({
  request: vi.fn(), navigate: vi.fn(),
  workspace: { id: "workspace", name: "Research", role: "admin" },
}));
vi.mock("../../src/api", async original => ({ ...await original<object>(), apiJson: (...args: unknown[]) => fixture.request(...args) }));
vi.mock("react-router", async original => ({
  ...await original<object>(), useNavigate: () => fixture.navigate,
  useRouteLoaderData: (id: string) => id === "authenticated" ? { user: { id: "actor" } } : { workspace: fixture.workspace },
}));
vi.mock("../../src/shell/ShellPage", () => ({ ShellPage: ({ children }: { children: React.ReactNode }) => children }));
vi.mock("../../src/i18n", () => {
  const t = (key: string, options?: { value1?: string }) => options?.value1 ? `${key} ${options.value1}` : key;
  return { i18n: { resolvedLanguage: "en" }, t, useTranslation: () => ({ t }) };
});

const member = (id: string, role: string) => ({
  membershipId: id, userId: id, email: `${id}@example.test`, role, createdAt: "2026-11-10T00:00:00Z",
});
const invitation = (id: string, role: string) => ({
  id, email: `${id}@example.test`, role, status: "pending",
  createdAt: "2026-11-10T00:00:00Z", expiresAt: "2026-11-13T00:00:00Z",
});
beforeEach(() => {
  fixture.workspace.role = "admin";
  fixture.request.mockReset(); fixture.navigate.mockReset();
  fixture.request.mockImplementation(async (path: string, options?: { method?: string; body?: string }) => {
    if (options?.method === "POST") {
      const body = JSON.parse(options.body!);
      return { invitation: { ...invitation("new", body.role), email: body.email }, inviteUrl: "https://example.test/activate#token=issued" };
    }
    if (options?.method === "PATCH") return { member: { ...member("reader", "member"), ...JSON.parse(options.body!) } };
    if (options?.method === "DELETE") return { ok: true };
    if (path.endsWith("/members")) return { members: [
      ...(fixture.workspace.role === "owner" ? [] : [member("owner", "owner")]),
      member("actor", fixture.workspace.role), member("peer", "admin"), member("reader", "member"),
    ] };
    if (path.endsWith("/invitations")) return { invitations: [invitation("admin-invite", "admin"), invitation("member-invite", "member")] };
    throw new Error(`Unexpected request: ${path}`);
  });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

async function openMembers(role = "admin") {
  fixture.workspace.role = role;
  render(<MemoryRouter><WorkspaceMembersRoute embedded /></MemoryRouter>);
  await screen.findByText("peer@example.test");
}

test("administrators only get actions for ordinary members and member invitations", async () => {
  const user = userEvent.setup();
  await openMembers();
  expect(screen.queryAllByRole("combobox")).toHaveLength(0);
  expect(screen.queryByRole("button", { name: "workspaceMembersRoute.memberActionsForValue peer@example.test" })).toBeNull();
  expect(screen.queryByRole("button", { name: "workspaceMembersRoute.memberActionsForValue owner@example.test" })).toBeNull();
  expect(screen.queryByRole("button", { name: "workspaceMembersRoute.memberActionsForValue actor@example.test" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "workspaceMembersRoute.memberActionsForValue reader@example.test" }));
  await user.click(screen.getByRole("button", { name: "workspaceMembersRoute.removeMember" }));
  const remove = screen.getByRole("dialog", { name: "workspaceMembersRoute.removeMember" });
  expect(remove.textContent).toContain("reader@example.test");
  await user.click(within(remove).getByRole("button", { name: "globalPluginSettings.remove" }));
  await waitFor(() => expect(screen.queryByText("reader@example.test")).toBeNull());
  expect(fixture.request).toHaveBeenCalledWith("/api/workspaces/workspace/members/reader", { method: "DELETE" });

  expect(screen.getAllByRole("button", { name: "workspaceMembersRoute.reissue" })).toHaveLength(1);
  expect(screen.getAllByRole("button", { name: "workspaceMembersRoute.revoke" })).toHaveLength(1);
  await user.click(screen.getByRole("button", { name: "workspaceMembersRoute.reissue" }));
  expect(screen.getByRole("dialog", { name: "workspaceMembersRoute.reissueInvitation" }).textContent).toContain("member-invite@example.test");
});

test("administrator invitations only offer and submit the member role", async () => {
  const user = userEvent.setup();
  await openMembers();
  await user.click(screen.getByRole("button", { name: "workspaceMembersRoute.inviteMember" }));
  const dialog = within(screen.getByRole("dialog", { name: "workspaceMembersRoute.inviteMember" }));
  expect(dialog.getAllByRole("option").map(option => (option as HTMLOptionElement).value)).toEqual(["member"]);
  await user.type(dialog.getByRole("textbox", { name: "loginForm.email" }), "new@example.test");
  await user.click(dialog.getByRole("button", { name: "workspaceMembersRoute.createInvitationLink" }));
  await screen.findByRole("dialog", { name: "workspaceMembersRoute.invitationLinkCreated" });
  const [, options] = fixture.request.mock.calls.find(([, options]) => options?.method === "POST")!;
  expect(JSON.parse(options.body)).toEqual({ email: "new@example.test", role: "member" });
});

test("owners can promote members, remove administrators and invite either lower role", async () => {
  const user = userEvent.setup();
  await openMembers("owner");
  const role = screen.getByRole("combobox", { name: "workspaceMembersRoute.roleForValue reader@example.test" });
  expect(within(role).getAllByRole("option").map(option => (option as HTMLOptionElement).value)).toEqual(["admin", "member"]);
  await user.selectOptions(role, "admin");
  await waitFor(() => expect((role as HTMLSelectElement).value).toBe("admin"));
  const [, options] = fixture.request.mock.calls.find(([, options]) => options?.method === "PATCH")!;
  expect(JSON.parse(options.body)).toEqual({ role: "admin" });

  const actions = screen.getByRole("button", { name: "workspaceMembersRoute.memberActionsForValue peer@example.test" });
  await user.click(actions);
  const menu = within(actions.closest("details")!);
  expect(menu.getByRole("button", { name: "workspaceMembersRoute.transferOwnership" })).toBeTruthy();
  await user.click(menu.getByRole("button", { name: "workspaceMembersRoute.removeMember" }));
  const remove = within(screen.getByRole("dialog", { name: "workspaceMembersRoute.removeMember" }));
  await user.click(remove.getByRole("button", { name: "globalPluginSettings.remove" }));
  await waitFor(() => expect(screen.queryByText("peer@example.test")).toBeNull());
  expect(screen.getAllByRole("button", { name: "workspaceMembersRoute.reissue" })).toHaveLength(2);
  expect(screen.getAllByRole("button", { name: "workspaceMembersRoute.revoke" })).toHaveLength(2);
  await user.click(screen.getByRole("button", { name: "workspaceMembersRoute.inviteMember" }));
  const dialog = within(screen.getByRole("dialog", { name: "workspaceMembersRoute.inviteMember" }));
  expect(dialog.getAllByRole("option").map(option => (option as HTMLOptionElement).value)).toEqual(["member", "admin"]);
});

test("a rejected invitation replacement explains the owner requirement and retains the draft", async () => {
  const user = userEvent.setup();
  await openMembers();
  fixture.request.mockRejectedValueOnce(new Error("workspace_invitation_management_forbidden"));
  await user.click(screen.getByRole("button", { name: "workspaceMembersRoute.inviteMember" }));
  const dialog = within(screen.getByRole("dialog", { name: "workspaceMembersRoute.inviteMember" }));
  const email = dialog.getByRole("textbox", { name: "loginForm.email" }) as HTMLInputElement;
  await user.type(email, "admin-invite@example.test");
  await user.click(dialog.getByRole("button", { name: "workspaceMembersRoute.createInvitationLink" }));
  expect((await screen.findByRole("alert")).textContent).toBe("workspaceMembersRoute.onlyOwnersCanManageAdministratorInvitations");
  expect(email.value).toBe("admin-invite@example.test");
  expect(screen.getByText("admin-invite@example.test")).toBeTruthy();
});
