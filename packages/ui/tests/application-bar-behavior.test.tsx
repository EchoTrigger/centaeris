// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { ApplicationBar } from "../src/components/ApplicationBar";
import { changeThemeAnimated } from "../src/theme";
vi.mock("../src/theme", () => ({ useThemePreference: () => "light", useResolvedTheme: () => "light", changeThemeAnimated: vi.fn() }));
vi.mock("../src/host/hostBridge", () => ({ isNativeHostRuntime: () => false, invokeHost: vi.fn() }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });
test("the titlebar keeps theme switching available when the sidebar is collapsed", async () => {
  const user = userEvent.setup();
  render(<ApplicationBar expanded={false} onToggle={vi.fn()} onBack={vi.fn()} onForward={vi.fn()} canBack={false} canForward={false} onError={vi.fn()} />);
  const theme = screen.getByRole("button", { name: "Switch to dark theme" });
  expect(theme.closest("header")).not.toBeNull();
  await user.click(theme);
  expect(changeThemeAnimated).toHaveBeenCalledExactlyOnceWith("dark", expect.objectContaining({ x: expect.any(Number), y: expect.any(Number) }));
  expect((screen.getByRole("button", { name: "Back" }) as HTMLButtonElement).disabled).toBe(true);
});
