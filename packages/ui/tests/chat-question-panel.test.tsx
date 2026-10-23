// @vitest-environment jsdom
import { useState } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { PendingQuestionPanel } from "../src/components/chat/ChatPendingPanels";
import type { PendingQuestionState } from "../src/components/chat/types";
afterEach(cleanup);
const initial = (multiSelect: boolean): PendingQuestionState => ({ assistantMessageId: "assistant", request: { id: "question", question: "Which items should remain?", options: ["Keep files", "Keep services"], multiSelect, required: true }, selectedOptions: [], answerText: "", submitting: false });
function Question({ multiSelect = true, submitting = false, onSubmit }: { multiSelect?: boolean; submitting?: boolean; onSubmit(): void }) {
  const [state, setState] = useState(() => initial(multiSelect));
  return <PendingQuestionPanel pendingQuestion={{ ...state, submitting }} pendingQuestionError="" onOptionToggle={option => setState(previous => ({ ...previous, selectedOptions: multiSelect ? previous.selectedOptions.includes(option) ? previous.selectedOptions.filter(value => value !== option) : [...previous.selectedOptions, option] : [option] }))} onTextChange={answerText => setState(previous => ({ ...previous, answerText }))} onSubmit={onSubmit} />;
}
test("question choices support keyboard selection, supplementary text and submission", async () => {
  const user = userEvent.setup(), submit = vi.fn();
  render(<Question onSubmit={submit} />);
  const files = screen.getByRole("checkbox", { name: "Keep files" });
  await user.click(files);
  expect(files.getAttribute("aria-checked")).toBe("true");
  await user.keyboard(" ");
  expect(files.getAttribute("aria-checked")).toBe("false");
  await user.type(screen.getByRole("textbox", { name: "Additional answer" }), "Preserve the workspace");
  expect((screen.getByRole("textbox", { name: "Additional answer" }) as HTMLTextAreaElement).value).toBe("Preserve the workspace");
  await user.click(screen.getByRole("button", { name: "Submit answer" }));
  expect(submit).toHaveBeenCalledOnce();
});
test("single-choice selection is exclusive and a pending answer disables every editable control", async () => {
  const user = userEvent.setup(), submit = vi.fn();
  const view = render(<Question multiSelect={false} onSubmit={submit} />);
  await user.click(screen.getByRole("radio", { name: "Keep files" }));
  await user.click(screen.getByRole("radio", { name: "Keep services" }));
  expect((screen.getByRole("radio", { name: "Keep files" }) as HTMLInputElement).checked).toBe(false);
  expect((screen.getByRole("radio", { name: "Keep services" }) as HTMLInputElement).checked).toBe(true);
  view.rerender(<Question multiSelect={false} submitting onSubmit={submit} />);
  expect((screen.getByRole("radio", { name: "Keep files" }) as HTMLInputElement).disabled).toBe(true);
  expect((screen.getByRole("textbox", { name: "Additional answer" }) as HTMLTextAreaElement).disabled).toBe(true);
  const button = screen.getByRole("button", { name: "Submitting…" });
  await user.click(button);
  expect(submit).not.toHaveBeenCalled();
});
