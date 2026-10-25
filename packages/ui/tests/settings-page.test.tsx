// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { useState } from "react";
import { SettingsNavigation, SettingsPage, type SettingsSection } from "../src/components/SettingsPage";
import { changeTheme } from "../src/theme";
vi.mock("../src/theme", () => ({ useThemePreference: () => "system", changeTheme: vi.fn() }));
afterEach(cleanup);

test("settings expose selected theme choices and model navigation without a duplicate heading", async () => {
  const user = userEvent.setup();
  function Harness() {
    const [section, setSection] = useState<SettingsSection>("appearance");
    return <><SettingsNavigation section={section} onSelect={setSection} collapsed={false} /><SettingsPage section={section}><p>Model content</p></SettingsPage></>;
  }
  render(<Harness />);
  expect(screen.getAllByRole("complementary")).toHaveLength(1);
  expect(screen.queryByRole("button", { name: "Back to Centaeris" })).toBeNull();
  expect(screen.getAllByRole("heading", { level: 1 }).map(heading => heading.textContent)).toEqual(["Appearance"]);
  const choices = within(screen.getByRole("group", { name: "Theme" }));
  expect(choices.getAllByRole("button").map(button => button.textContent)).toEqual(["System", "Light", "Dark"]);
  expect(choices.getByRole("button", { name: "System" }).getAttribute("aria-pressed")).toBe("true");
  await user.click(choices.getByRole("button", { name: "Dark" }));
  expect(changeTheme).toHaveBeenCalledWith("dark");
  await user.click(within(screen.getByRole("navigation", { name: "Settings categories" })).getByRole("button", { name: "Model services" }));
  expect(screen.getByText("Model content")).toBeDefined();
});

test("model configuration navigation opens the requested section and can change while mounted", async () => {
  const renderSection = (section: SettingsSection) => <><SettingsNavigation section={section} onSelect={() => {}} collapsed={false} /><SettingsPage section={section}><h1>Model content</h1></SettingsPage></>;
  const { rerender } = render(renderSection("appearance"));
  expect(screen.getByRole("heading", { name: "Appearance" })).toBeDefined();
  rerender(renderSection("models"));
  expect(await screen.findByRole("heading", { name: "Model content" })).toBeDefined();
  expect(screen.getByRole("button", { name: "Model services" }).getAttribute("aria-current")).toBe("page");
});

test("collapsed settings navigation leaves the active content available", () => {
  render(<><SettingsNavigation section="appearance" onSelect={() => {}} collapsed /><SettingsPage section="appearance">Model content</SettingsPage></>);
  expect(screen.queryByRole("navigation", { name: "Settings categories" })).toBeNull();
  expect(screen.getByRole("heading", { name: "Appearance" })).toBeTruthy();
});
