// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { IssueForm } from "../../src/work/IssueForm";
import { i18n } from "../../src/i18n";

beforeEach(async () => { await i18n.changeLanguage("en"); });
afterEach(cleanup);

const definition = { fields: [{ key: "start", label: "Start date", type: "date", required: true },
  { key: "entries", label: "Expense items", type: "items", fields: [
    { key: "amount", label: "Amount", type: "money", required: true, minValue: "0.01" },
  ] }] };

test("an installed template creates structured properties through the shared issue form", async () => {
  const save = vi.fn();
  const user = userEvent.setup();
  render(<IssueForm assignees={[]} templates={[{ key: "request", name: "Request", definition }]} initialTemplateKey="request" onSave={save} onCancel={() => {}} />);
  await user.type(screen.getByRole("textbox", { name: "Title" }), "Travel request");
  fireEvent.change(screen.getByLabelText("Start date"), { target: { value: "2026-11-13" } });
  await user.click(screen.getByRole("button", { name: "Add item" }));
  await user.type(screen.getByRole("spinbutton", { name: "Amount" }), "12.50");
  await user.click(screen.getByRole("button", { name: "Create issue" }));
  expect(save).toHaveBeenCalledWith(expect.objectContaining({ templateKey: "request", properties: { start: "2026-11-13", entries: [{ amount: "12.5" }] } }));
});

test("editing retains the captured template and removes only the selected item", async () => {
  const save = vi.fn();
  const user = userEvent.setup();
  const issue = { title: "Existing", templateKey: "request", templateSnapshot: { definition }, properties: { start: "2026-11-13", entries: [{ amount: "5.00" }, { amount: "8.00" }] } };
  render(<IssueForm issue={issue} assignees={[]} templates={[]} onSave={save} onCancel={() => {}} />);
  await user.click(screen.getByRole("button", { name: "Remove item 1" }));
  await user.click(screen.getByRole("button", { name: "Save changes" }));
  expect(save).toHaveBeenCalledWith(expect.objectContaining({ properties: { start: "2026-11-13", entries: [{ amount: "8.00" }] } }));
  expect(save.mock.calls[0][0]).not.toHaveProperty("templateKey");
});
