// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { apiUrl, configureApi } from "../../src/api";
import { HomePlane } from "../../src/shell/HomePlane";

vi.mock("../../src/i18n", () => {
  const t = (key: string) => key;
  return { t, useTranslation: () => ({ t }) };
});

beforeEach(() => configureApi({ apiBaseUrl: window.location.origin }));
afterEach(cleanup);

test("home shows the workspace avatar, follows name and image changes, and returns to its initial after removal", () => {
  const workspace = { name: "Research", avatarUrl: `/api/workspaces/workspace/avatar/${"a".repeat(64)}` };
  const { container, rerender } = render(<HomePlane workspace={workspace} />);
  expect(screen.getByRole("heading", { name: "Research" })).toBeTruthy();
  expect(container.querySelector("img")?.getAttribute("src")).toBe(apiUrl(workspace.avatarUrl));

  const updated = { name: "Design", avatarUrl: `/api/workspaces/workspace/avatar/${"b".repeat(64)}` };
  rerender(<HomePlane workspace={updated} />);
  expect(screen.getByRole("heading", { name: "Design" })).toBeTruthy();
  expect(container.querySelector("img")?.getAttribute("src")).toBe(apiUrl(updated.avatarUrl));

  rerender(<HomePlane workspace={{ name: "Design", avatarUrl: null }} />);
  expect(container.querySelector("img")).toBeNull();
  expect(screen.getByText("D")).toBeTruthy();
  expect(screen.getByRole("heading", { name: "Design" })).toBeTruthy();
});
