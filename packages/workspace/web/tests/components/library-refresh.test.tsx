// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { apiJson } from "../../src/api";
import { i18n } from "../../src/i18n";
import LibraryPage from "../../src/routes/LibraryRoute";

vi.mock("../../src/api", async original => ({ ...await original<typeof import("../../src/api")>(), apiJson: vi.fn() }));
vi.mock("react-router", async original => ({ ...await original<typeof import("react-router")>(),
  useRouteLoaderData: (id: string) => id === "authenticated" ? { user: { id: "user" } } : { workspace: { id: "one", name: "Research", role: "owner" } },
}));
const file = { id: "report", displayName: "Report.pdf", objectKind: "file", contentType: "application/pdf", status: "ready", sizeBytes: 1200, updatedAt: "2026-11-12T00:00:00Z" };
beforeEach(async () => { await i18n.changeLanguage("en"); });
afterEach(() => { cleanup(); vi.clearAllMocks(); });

test("creating a folder keeps existing files visible during the directory refresh", async () => {
  let refreshing = false;
  let release: (() => void) | undefined;
  vi.mocked(apiJson).mockImplementation(async (path, options = {}) => {
    if (path === "/api/library/folders" && options.method === "POST") { refreshing = true; return { object: { id: "new-folder", objectKind: "folder", displayName: "New folder" } }; }
    if (path === "/api/library") return refreshing ? new Promise(resolve => { release = () => resolve({ objects: [file] }); }) : { objects: [file] };
    throw new Error(`Unexpected library request: ${path}`);
  });
  const user = userEvent.setup();
  render(<MemoryRouter initialEntries={["/w/one/library"]}><LibraryPage embedded /></MemoryRouter>);
  await screen.findByRole("cell", { name: "Report.pdf" });
  await user.click(screen.getByRole("button", { name: "New", exact: true }));
  await user.click(await screen.findByRole("menuitem", { name: "Folder", exact: true }));
  await user.type(screen.getByPlaceholderText("Folder name"), "New folder");
  await user.click(screen.getByRole("button", { name: "Create", exact: true }));
  await waitFor(() => expect(release).toBeTypeOf("function"));
  expect(screen.getByRole("cell", { name: "Report.pdf" })).toBeTruthy();
  await act(async () => { release?.(); });
  expect(screen.getByRole("cell", { name: "Report.pdf" })).toBeTruthy();
});
