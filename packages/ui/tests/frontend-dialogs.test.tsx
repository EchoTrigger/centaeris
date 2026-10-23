// @vitest-environment jsdom
import { useState } from "react";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { ConfirmDialog } from "../src/components/ConfirmDialog";
import { ResourceDetailDialog } from "../src/components/ResourceDetailDialog";
afterEach(cleanup);

test("confirmation retains safe initial focus, confirmation, cancellation and Escape", async () => {
  const user = userEvent.setup(), confirm = vi.fn(), cancel = vi.fn();
  function Harness() {
    const [open, setOpen] = useState(false);
    return <><button onClick={() => setOpen(true)}>Remove resource</button>
      <ConfirmDialog open={open} title="Remove resource?" message="Cannot be undone"
        onCancel={() => { cancel(); setOpen(false); }} onConfirm={() => { confirm(); setOpen(false); }} /></>;
  }
  render(<Harness />);
  const trigger = screen.getByRole("button", { name: "Remove resource" });
  await user.click(trigger);
  const dialog = await screen.findByRole("alertdialog", { name: "Remove resource?" });
  expect(dialog.getAttribute("aria-describedby")).toBeTruthy();
  await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("button", { name: "Cancel" })));
  await user.click(screen.getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
  expect(cancel).toHaveBeenCalledOnce();
  expect(confirm).not.toHaveBeenCalled();
  await user.click(trigger);
  await screen.findByRole("alertdialog");
  await user.keyboard("{Escape}");
  await waitFor(() => expect(cancel).toHaveBeenCalledTimes(2));
  await user.click(trigger);
  await user.click(await screen.findByRole("button", { name: "Confirm" }));
  expect(confirm).toHaveBeenCalledOnce();
});

test("resource detail closes with Escape and restores focus to its opener", async () => {
  const user = userEvent.setup();
  function Harness() {
    const [open, setOpen] = useState(false);
    return <><button onClick={() => setOpen(true)}>Open model</button>{open ? <ResourceDetailDialog title="Model details" onClose={() => setOpen(false)}><p>Model configuration</p></ResourceDetailDialog> : null}</>;
  }
  render(<Harness />);
  const trigger = screen.getByRole("button", { name: "Open model" });
  await user.click(trigger);
  await screen.findByRole("dialog", { name: "Model details" });
  await user.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(document.activeElement).toBe(trigger);
});
