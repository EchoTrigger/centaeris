// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { FileLinkCard } from "../../src/chat/FileLinkCard";

afterEach(cleanup);

test("a script card opens its preview with the published file's navigation anchor", async () => {
  const open = vi.fn();
  render(<FileLinkCard filename="fix-pagefile-dump.ps1" href="/download/script" id="artifact:script" onClick={event => {
    event.preventDefault();
    open(event.currentTarget.id);
  }} />);
  expect(screen.getByText("PS1")).toBeTruthy();
  await userEvent.setup().click(screen.getByRole("link", { name: "fix-pagefile-dump.ps1" }));
  expect(open).toHaveBeenCalledExactlyOnceWith("artifact:script");
});

test("a downloadable card retains its file URL and native download behavior", () => {
  render(<FileLinkCard filename="Report.pdf" href="/download/report" download />);
  const link = screen.getByRole("link", { name: "Report.pdf" });
  expect(link.getAttribute("href")).toBe("/download/report");
  expect(link.hasAttribute("download")).toBe(true);
});
