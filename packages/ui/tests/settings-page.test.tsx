// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { SettingsPage } from "../src/components/SettingsPage";
import { changeTheme } from "../src/theme";
vi.mock("../src/theme", () => ({ useThemePreference: () => "system", changeTheme: vi.fn() }));
afterEach(cleanup);

test("settings expose selected theme choices and model navigation without a duplicate heading", async () => {
  const user = userEvent.setup();
  render(<SettingsPage><p>Model content</p></SettingsPage>);
  expect(screen.getAllByRole("heading", { level: 1 }).map(heading => heading.textContent)).toEqual(["Appearance"]);
  const choices = within(screen.getByRole("group", { name: "Theme" }));
  expect(choices.getAllByRole("button").map(button => button.textContent)).toEqual(["SystemFollow your device", "LightA bright workspace", "DarkA quieter workspace"]);
  expect(choices.getByRole("button", { name: "System" }).getAttribute("aria-pressed")).toBe("true");
  await user.click(choices.getByRole("button", { name: "Dark" }));
  expect(changeTheme).toHaveBeenCalledWith("dark");
  await user.click(within(screen.getByRole("navigation", { name: "Settings categories" })).getByRole("button", { name: "Model services" }));
  expect(screen.getByText("Model content")).toBeDefined();
});

test("model configuration navigation opens the requested section and can change while mounted", async () => {
  const { rerender } = render(<SettingsPage><h1>Model content</h1></SettingsPage>);
  expect(screen.getByRole("heading", { name: "Appearance" })).toBeDefined();
  rerender(<SettingsPage initialSection="models"><h1>Model content</h1></SettingsPage>);
  expect(await screen.findByRole("heading", { name: "Model content" })).toBeDefined();
  expect(screen.getByRole("button", { name: "Model services" }).getAttribute("aria-current")).toBe("page");
});
