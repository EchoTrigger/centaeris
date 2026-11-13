// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { ApiError, apiJson } from "../../src/api";
import { i18n } from "../../src/i18n";
import { SpaceMaterialView } from "../../src/routes/SpaceMaterialView";

vi.mock("../../src/api", async original => ({ ...await original<typeof import("../../src/api")>(), apiJson: vi.fn() }));
beforeEach(async () => { await i18n.changeLanguage("en"); });
afterEach(() => { cleanup(); vi.clearAllMocks(); });

test("material results open the matched lines and remove content after permission is revoked", async () => {
  const user = userEvent.setup();
  vi.mocked(apiJson).mockImplementation(async path => {
    if (path.includes("offset=5")) throw new ApiError("material_not_found", 404);
    return { material: { displayName: "Policy" }, representationId: "rep", content: "Matched rule\n", startLine: 4, endLine: 5, nextOffset: 5, totalLines: 9 };
  });
  render(<MemoryRouter><SpaceMaterialView workspaceId="one" sourceId="source" objectId="object" representationId="rep" offset={3} highlight="rule" /></MemoryRouter>);
  await screen.findByRole("heading", { name: "Policy" });
  expect(vi.mocked(apiJson).mock.calls[0][0]).toContain("offset=3");
  expect(vi.mocked(apiJson).mock.calls[0][0]).toContain("representationId=rep");
  expect(screen.getByText("rule").tagName).toBe("MARK");
  await user.click(screen.getByRole("button", { name: "Load more materials and results" }));
  await screen.findByRole("alert");
  expect(screen.queryByText("rule")).toBeNull();
  expect(screen.getByRole("link").getAttribute("href")).toContain("source=source");
});
