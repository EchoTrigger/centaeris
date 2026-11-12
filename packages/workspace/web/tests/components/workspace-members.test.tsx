// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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
afterEach(() => { cleanup(); vi.useRealTimers(); vi.restoreAllMocks(); });

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
  vi.useFakeTimers();
  await act(async () => { fireEvent.submit(dialog.getByRole("button", { name: "workspaceMembersRoute.createInvitationLink" }).closest("form")!); });
  const message = "workspaceMembersRoute.onlyOwnersCanManageAdministratorInvitations";
  expect(dialog.getByRole("alert").textContent).toBe(message);
  await act(async () => { vi.advanceTimersByTime(5000); });
  expect(dialog.getByRole("alert").textContent).toBe(message);
  vi.useRealTimers();
  expect(email.value).toBe("admin-invite@example.test");
  expect(screen.getByText("admin-invite@example.test")).toBeTruthy();
  await user.clear(email);
  await user.type(email, "reader-invite@example.test");
  expect(dialog.queryByRole("alert")).toBeNull();
  await user.click(dialog.getByRole("button", { name: "workspaceMembersRoute.createInvitationLink" }));
  await screen.findByRole("dialog", { name: "workspaceMembersRoute.invitationLinkCreated" });
});

test.each(["remove", "reissue", "revoke"])("%s failures stay in their confirmation and allow retry", async action => {
  const user = userEvent.setup();
  await openMembers();
  if (action === "remove") {
    await user.click(screen.getByRole("button", { name: "workspaceMembersRoute.memberActionsForValue reader@example.test" }));
    await user.click(screen.getByRole("button", { name: "workspaceMembersRoute.removeMember" }));
  } else {
    await user.click(screen.getByRole("button", { name: `workspaceMembersRoute.${action}` }));
  }
  const title = `workspaceMembersRoute.${action === "remove" ? "removeMember" : `${action}Invitation`}`;
  const dialog = within(screen.getByRole("dialog", { name: title }));
  const confirmName = action === "remove" ? "globalPluginSettings.remove" : `workspaceMembersRoute.${action}`;
  fixture.request.mockRejectedValueOnce(new Error("offline"));
  await user.click(dialog.getByRole("button", { name: confirmName }));
  expect((await dialog.findByRole("alert")).textContent).toBe("offline");
  expect(screen.getAllByRole("alert")).toHaveLength(1);
  await user.click(dialog.getByRole("button", { name: confirmName }));
  await waitFor(() => expect(screen.queryByRole("dialog", { name: title })).toBeNull());
  expect(screen.queryByRole("alert")).toBeNull();
});

test("a pending invitation retains its input and cannot be dismissed or edited", async () => {
  const user = userEvent.setup();
  await openMembers();
  await user.click(screen.getByRole("button", { name: "workspaceMembersRoute.inviteMember" }));
  const dialog = within(screen.getByRole("dialog", { name: "workspaceMembersRoute.inviteMember" }));
  const email = dialog.getByRole("textbox") as HTMLInputElement;
  await user.type(email, "new@example.test");
  let reject!: (error: Error) => void;
  fixture.request.mockImplementationOnce(() => new Promise((_resolve, rejectRequest) => { reject = rejectRequest; }));
  await user.click(dialog.getByRole("button", { name: "workspaceMembersRoute.createInvitationLink" }));
  expect(email.disabled).toBe(true);
  expect((dialog.getByRole("combobox") as HTMLSelectElement).disabled).toBe(true);
  await user.keyboard("{Escape}");
  expect(screen.getByRole("dialog", { name: "workspaceMembersRoute.inviteMember" })).toBeTruthy();
  await act(async () => { reject(new Error("offline")); });
  expect(dialog.getByRole("alert").textContent).toBe("offline");
  expect(email.value).toBe("new@example.test");
  expect(email.disabled).toBe(false);
  await user.click(dialog.getByRole("button", { name: "agentRunRow.cancel" }));
  await user.click(screen.getByRole("button", { name: "workspaceMembersRoute.inviteMember" }));
  expect(screen.queryByRole("alert")).toBeNull();
  expect((screen.getByRole("textbox") as HTMLInputElement).value).toBe("");
});

test("role-change errors persist on the page and retain the original role", async () => {
  await openMembers("owner");
  vi.useFakeTimers();
  fixture.request.mockRejectedValueOnce(new Error("workspace_member_management_forbidden"));
  const role = screen.getByRole("combobox", { name: "workspaceMembersRoute.roleForValue reader@example.test" }) as HTMLSelectElement;
  await act(async () => { fireEvent.change(role, { target: { value: "admin" } }); });
  await act(async () => { vi.advanceTimersByTime(5000); });
  expect(screen.getByRole("alert").textContent).toBe("workspaceMembersRoute.onlyOwnersCanManageAdministrators");
  expect(role.value).toBe("member");
});

test("clipboard failure stays with the issued link and can be retried", async () => {
  const user = userEvent.setup();
  await openMembers();
  await user.click(screen.getByRole("button", { name: "workspaceMembersRoute.inviteMember" }));
  await user.type(screen.getByRole("textbox"), "new@example.test");
  await user.click(screen.getByRole("button", { name: "workspaceMembersRoute.createInvitationLink" }));
  const dialog = within(await screen.findByRole("dialog", { name: "workspaceMembersRoute.invitationLinkCreated" }));
  let reject!: (error: Error) => void;
  const write = vi.spyOn(navigator.clipboard, "writeText").mockImplementationOnce(() => new Promise((_resolve, rejectRequest) => { reject = rejectRequest; }));
  const copy = dialog.getByRole("button", { name: "workspaceMembersRoute.copyLink" }) as HTMLButtonElement;
  await user.click(copy);
  expect(copy.disabled).toBe(true);
  expect((dialog.getByRole("button", { name: "workspaceMembersRoute.done" }) as HTMLButtonElement).disabled).toBe(true);
  await user.keyboard("{Escape}");
  expect(screen.getByRole("dialog", { name: "workspaceMembersRoute.invitationLinkCreated" })).toBeTruthy();
  await act(async () => { reject(new Error("denied")); });
  expect(dialog.getByRole("alert").textContent).toBe("workspaceMembersRoute.unableToCopyInvitationLinkValue denied");
  expect(dialog.getByText("https://example.test/activate#token=issued")).toBeTruthy();
  write.mockResolvedValueOnce(undefined);
  await user.click(copy);
  expect(write).toHaveBeenLastCalledWith("https://example.test/activate#token=issued");
  expect(dialog.queryByRole("alert")).toBeNull();
  expect(screen.getByRole("status").textContent).toBe("workspaceMembersRoute.invitationLinkCopied");
});

test("an ownership-transfer rejection keeps the password in its dialog until cancellation", async () => {
  const user = userEvent.setup();
  await openMembers("owner");
  const actions = screen.getByRole("button", { name: "workspaceMembersRoute.memberActionsForValue peer@example.test" });
  await user.click(actions);
  await user.click(within(actions.closest("details")!).getByRole("button", { name: "workspaceMembersRoute.transferOwnership" }));
  const dialog = within(screen.getByRole("dialog", { name: "workspaceMembersRoute.transferWorkspaceOwnership" }));
  const password = dialog.getByLabelText("settingsRoute.currentPassword") as HTMLInputElement;
  await user.type(password, "incorrect password");
  fixture.request.mockRejectedValueOnce(new Error("workspace_owner_reauthentication_failed"));
  await user.click(dialog.getByRole("button", { name: "workspaceMembersRoute.confirmTransfer" }));
  expect((await dialog.findByRole("alert")).textContent).toBe("settingsRoute.theCurrentPasswordIsIncorrect");
  expect(password.value).toBe("incorrect password");
  await user.click(dialog.getByRole("button", { name: "agentRunRow.cancel" }));
  await user.click(actions);
  await user.click(within(actions.closest("details")!).getByRole("button", { name: "workspaceMembersRoute.transferOwnership" }));
  expect(screen.queryByRole("alert")).toBeNull();
  expect((screen.getByLabelText("settingsRoute.currentPassword") as HTMLInputElement).value).toBe("");
});
