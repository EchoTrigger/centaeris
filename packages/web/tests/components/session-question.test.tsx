// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { SessionQuestionCard } from "../../src/chat/SessionQuestionCard";

vi.mock("../../src/i18n", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
afterEach(cleanup);
const question = { question: "How should the results be delivered?", options: ["Summary", "Task list"] };

test("collapse only hides the question and preserves its answer draft", async () => {
  const user = userEvent.setup();
  const answer = vi.fn();
  render(<SessionQuestionCard question={question} onAnswer={answer} />);
  await user.click(screen.getByRole("button", { name: /1 Summary/ }));
  await user.type(screen.getByRole("textbox"), "Include evidence");
  await user.click(screen.getByRole("button", { name: "sessionQuestion.collapse" }));
  expect(screen.queryByRole("textbox")).toBeNull();
  expect(answer).not.toHaveBeenCalled();
  await user.click(screen.getByRole("button", { name: "sessionQuestion.expand" }));
  expect(screen.getByRole("textbox").getAttribute("disabled")).toBeNull();
  expect((screen.getByRole("textbox") as HTMLTextAreaElement).value).toBe("Include evidence");
  await user.click(screen.getByRole("button", { name: "sessionQuestion.send" }));
  expect(answer).toHaveBeenCalledWith({ action: "answer", answer: "1. Summary\n\nInclude evidence" });
});

test("sending prevents duplicate answers and skip is an explicit separate action", async () => {
  const user = userEvent.setup();
  let resolve!: () => void;
  const answer = vi.fn(() => new Promise<void>(done => { resolve = done; }));
  const view = render(<SessionQuestionCard question={question} onAnswer={answer} />);
  expect((screen.getByRole("button", { name: "sessionQuestion.send" }) as HTMLButtonElement).disabled).toBe(true);
  await user.type(screen.getByRole("textbox"), "Free reply");
  await user.dblClick(screen.getByRole("button", { name: "sessionQuestion.send" }));
  expect(answer).toHaveBeenCalledTimes(1);
  expect(answer).toHaveBeenCalledWith({ action: "answer", answer: "Free reply" });
  expect((screen.getByRole("button", { name: "sessionQuestion.skip" }) as HTMLButtonElement).disabled).toBe(true);
  await act(async () => resolve());
  view.unmount();
  render(<SessionQuestionCard question={question} onAnswer={answer} />);
  await user.click(screen.getByRole("button", { name: "sessionQuestion.skip" }));
  expect(answer).toHaveBeenLastCalledWith({ action: "skip" });
  await act(async () => resolve());
});

test("a failed answer keeps the question and permits retry", async () => {
  const user = userEvent.setup();
  const answer = vi.fn().mockRejectedValueOnce(new Error("Disconnected")).mockResolvedValue(undefined);
  render(<SessionQuestionCard question={question} onAnswer={answer} />);
  await user.click(screen.getByRole("button", { name: /2 Task list/ }));
  await user.click(screen.getByRole("button", { name: "sessionQuestion.send" }));
  expect(screen.getByRole("alert").textContent).toBe("sessionQuestion.failed");
  await user.click(screen.getByRole("button", { name: "sessionQuestion.send" }));
  expect(answer).toHaveBeenCalledTimes(2);
});
