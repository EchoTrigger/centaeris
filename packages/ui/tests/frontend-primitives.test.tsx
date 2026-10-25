// @vitest-environment jsdom
import { renderToStaticMarkup } from "react-dom/server";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { Button } from "../src/components/ui/button";
import { Input } from "../src/components/ui/input";
import { Textarea } from "../src/components/ui/textarea";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "../src/components/ui/tooltip";

afterEach(cleanup);

test("action buttons preserve labels, explicit form behavior and disabled state", () => {
  const html = renderToStaticMarkup(<form>
    <Button type="button" variant="composerSend" size="composerSend" aria-label="Send prompt" disabled>Send</Button>
    <Button type="submit">Save</Button>
  </form>);
  expect(html).toContain('type="button"');
  expect(html).toContain('aria-label="Send prompt"');
  expect(html).toContain('disabled=""');
  expect(html).toContain('type="submit"');
  expect(html).toContain("Save");
});

test("text controls retain their accessible names and controlled input callbacks", () => {
  const input = vi.fn(), textarea = vi.fn();
  render(<div>
    <Input aria-label="Name" value="Existing" onChange={(event) => input(event.target.value)} />
    <Textarea aria-label="Prompt" value="Draft" onChange={(event) => textarea(event.target.value)} />
  </div>);
  const name = screen.getByRole("textbox", { name: "Name" }) as HTMLInputElement;
  const prompt = screen.getByRole("textbox", { name: "Prompt" }) as HTMLTextAreaElement;
  expect(name.value).toBe("Existing");
  expect(prompt.value).toBe("Draft");
  fireEvent.change(name, { target: { value: "Updated" } });
  fireEvent.change(prompt, { target: { value: "Continue" } });
  expect(input).toHaveBeenCalledExactlyOnceWith("Updated");
  expect(textarea).toHaveBeenCalledExactlyOnceWith("Continue");
});

test("tooltips open from keyboard focus, retain the trigger label and dismiss with Escape", async () => {
  const user = userEvent.setup();
  render(<TooltipProvider delay={0}>
    <Tooltip><TooltipTrigger render={<Button aria-label="Copy answer">Copy</Button>} />
      <TooltipContent>Copy this answer</TooltipContent>
    </Tooltip>
  </TooltipProvider>);
  await user.tab();
  await screen.findByText("Copy this answer");
  expect(screen.getByRole("button", { name: "Copy answer" })).toBe(document.activeElement);
  await user.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByText("Copy this answer")).toBeNull());
});
