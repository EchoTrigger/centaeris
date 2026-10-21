// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { ChatActionsMenu } from "../src/components/ChatActionsMenu";
import { DropdownMenuItem } from "../src/components/ui/dropdown-menu";
afterEach(cleanup);

test("chat actions support keyboard selection, outside dismissal and Escape focus restoration", async () => {
  const user = userEvent.setup(), pin = vi.fn();
  render(<><button>Outside</button><ChatActionsMenu>
    <DropdownMenuItem onClick={pin}>Pin chat</DropdownMenuItem>
    <DropdownMenuItem disabled>Delete unavailable</DropdownMenuItem>
  </ChatActionsMenu></>);
  const trigger = screen.getByRole("button", { name: "Chat actions" });
  await user.click(trigger);
  await screen.findByRole("menu");
  await user.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  expect(document.activeElement).toBe(trigger);
  await user.keyboard("{ArrowDown}");
  await screen.findByRole("menu");
  await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "Pin chat" })));
  await user.keyboard("{Enter}");
  await waitFor(() => expect(pin).toHaveBeenCalledOnce());
  await user.click(trigger);
  await screen.findByRole("menu");
  await user.click(screen.getByRole("button", { name: "Outside" }));
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
});
