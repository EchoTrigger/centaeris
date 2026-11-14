// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { apiJson } from "../../src/api";
import { ModelDefaults } from "../../src/components/ModelDefaults";
import { i18n } from "../../src/i18n";

vi.mock("../../src/api", async original => ({ ...await original<typeof import("../../src/api")>(), apiJson: vi.fn() }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });
test("default purpose can be selected and cleared after acknowledged saves, without changing model reasoning", async () => {
  await i18n.changeLanguage("en");
  let defaults = { titleModelConfigRef: null as string | null, titleThinkingMode: "low" };
  vi.mocked(apiJson).mockImplementation(async (_path, options = {}) => {
    if (options.method === "PATCH") defaults = JSON.parse(String(options.body));
    return { defaults };
  });
  const model = { id: "fast", displayName: "Fast model", enabled: true, thinkingMode: "high", thinkingModes: ["low", "high"] };
  const user = userEvent.setup(); render(<ModelDefaults workspaceId="one" models={[model]} />);
  const select = await screen.findByRole("combobox", { name: "Default purpose" });
  await waitFor(() => expect((select as HTMLSelectElement).disabled).toBe(false));
  await user.selectOptions(select, "fast");
  await waitFor(() => expect(defaults).toEqual({ titleModelConfigRef: "fast", titleThinkingMode: "low" }));
  expect(model.thinkingMode).toBe("high");
  await user.selectOptions(select, "");
  await waitFor(() => expect(defaults.titleModelConfigRef).toBe(null));
});
test("a failed save retains the acknowledged default purpose", async () => {
  await i18n.changeLanguage("en");
  vi.mocked(apiJson).mockImplementation(async (_path, options = {}) => {
    if (options.method === "PATCH") throw new Error("save failed");
    return { defaults: { titleModelConfigRef: "fast", titleThinkingMode: "low" } };
  });
  const user = userEvent.setup(); render(<ModelDefaults workspaceId="one" models={[{ id: "fast", enabled: true, displayName: "Fast", thinkingModes: ["low"] }]} />);
  const select = await screen.findByRole("combobox", { name: "Default purpose" });
  await waitFor(() => expect((select as HTMLSelectElement).value).toBe("fast"));
  await user.selectOptions(select, "");
  await screen.findByRole("alert");
  expect((select as HTMLSelectElement).value).toBe("fast");
});
