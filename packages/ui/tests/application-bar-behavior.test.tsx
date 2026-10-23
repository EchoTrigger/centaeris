// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { ApplicationBar } from "../src/components/ApplicationBar";
vi.mock("../src/theme", () => ({ useThemePreference: () => "light" }));
vi.mock("../src/host/hostBridge", () => ({ isNativeHostRuntime: () => false, invokeHost: vi.fn() }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });
test("the collapsed titlebar keeps navigation and application menus without a theme shortcut", async () => {
  const user = userEvent.setup();
  const onToggle = vi.fn();
  render(<ApplicationBar expanded={false} onToggle={onToggle} onBack={vi.fn()} onForward={vi.fn()} canBack={false} canForward={false} onError={vi.fn()} />);
  expect(screen.getAllByRole("button").map(button => button.getAttribute("aria-label") ?? button.textContent)).toEqual(["Back", "Forward", "Show left sidebar", "File", "Edit", "View", "Help"]);
  expect((screen.getByRole("button", { name: "Back" }) as HTMLButtonElement).disabled).toBe(true);
  await user.click(screen.getByRole("button", { name: "Show left sidebar" }));
  expect(onToggle).toHaveBeenCalledOnce();
});
