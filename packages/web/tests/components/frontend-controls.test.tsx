// @vitest-environment jsdom
import { useState } from "react";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { ModelPicker, ThinkingPicker } from "../../src/components/ModelSelectionPickers";
import { ConfirmDialog } from "../../src/components/ConfirmDialog";
import { WorkspaceComposer } from "../../src/chat/WorkspaceComposer";

vi.mock("../../src/i18n", () => {
  const labels: Record<string, string> = {
    "appRoute.aiModel": "AI model", "appRoute.reasoningEffort": "Reasoning effort",
    "appRoute.selectReasoningEffort": "Choose effort", "appRoute.selectAiModel": "Choose model",
    "appRoute.low": "Low", "appRoute.high": "High", "agentRunRow.cancel": "Cancel",
    "confirmDialog.confirm": "Confirm",
  };
  const t = (key: string) => labels[key] ?? key;
  return { t, useTranslation: () => ({ t }) };
});

afterEach(cleanup);

const models = [
  { id: "a", displayName: "Model A", thinkingModes: ["low", "high"] },
  { id: "b", displayName: "Model B", thinkingModes: ["low", "high"] },
];
const groups = [{ provider: "provider", label: "Provider", models }];

const composerProps = {
  pendingAttachments: [], pendingUploadFiles: [], draft: "", onDraftChange: () => {},
  modelGroups: groups, modelId: "a", currentModel: models[0], thinkingMode: "high",
  fileInputRef: { current: null }, workspaceAvailable: true,
  sessionId: null,
};

test("empty send remains reachable by mouse and keyboard while the draft stays editable during submission", async () => {
  const user = userEvent.setup(), submit = vi.fn();
  function Harness() {
    const [draft, setDraft] = useState("");
    return <WorkspaceComposer {...composerProps} draft={draft} onDraftChange={setDraft} sending loadingHistory
      controlBusy onSubmit={event => { event.preventDefault(); submit(); }} />;
  }
  render(<Harness />);
  const send = screen.getByRole("button", { name: "appRoute.input" });
  const draft = screen.getByRole("textbox", { name: "appRoute.message" }) as HTMLTextAreaElement;
  await user.click(send);
  send.focus(); await user.keyboard("{Enter}"); await user.keyboard(" ");
  expect(submit).toHaveBeenCalledTimes(3);
  expect(send.getAttribute("aria-busy")).toBe("true");
  expect(screen.queryByRole("status")).toBeNull();
  await user.type(draft, "Next draft");
  expect(draft.value).toBe("Next draft");
  await user.keyboard("{Enter}");
  expect(submit).toHaveBeenCalledTimes(4);
});

test("Stop remains reachable by mouse and keyboard while its identity or cancellation is pending", async () => {
  const user = userEvent.setup(), stop = vi.fn();
  const { rerender } = render(<WorkspaceComposer {...composerProps} hasActiveAgentRun activeAgentRunId=""
    controlBusy onCancelActiveAgentRun={stop} />);
  const button = screen.getByRole("button", { name: "appRoute.stop" });
  await user.click(button);
  button.focus(); await user.keyboard("{Enter}"); await user.keyboard(" ");
  expect(stop).toHaveBeenCalledTimes(3);
  rerender(<WorkspaceComposer {...composerProps} hasActiveAgentRun activeAgentRunId="run" cancellingAgentRunId="run"
    onCancelActiveAgentRun={stop} />);
  await user.click(button);
  expect(stop).toHaveBeenCalledTimes(4);
  expect(button.getAttribute("aria-busy")).toBe("true");
  expect(screen.queryByRole("status")).toBeNull();
});

test("model and effort keep separate choices and preserve the draft", async () => {
  const user = userEvent.setup();
  const chooseModel = vi.fn(), chooseEffort = vi.fn();
  function Harness() {
    const [id, setId] = useState("a"), [effort, setEffort] = useState("high");
    const [draft, setDraft] = useState("Keep this draft");
    const model = models.find(item => item.id === id)!;
    return <><label>Draft<input value={draft} onChange={event => setDraft(event.target.value)} /></label>
      <ThinkingPicker model={model} value={effort} disabled={false} onChange={(next: string) => { chooseEffort(next); setEffort(next); }} />
      <ModelPicker groups={groups} model={model} value={id} disabled={false} onChange={(next: string) => { chooseModel(next); setId(next); }} />
    </>;
  }
  render(<Harness />);
  expect(chooseModel).not.toHaveBeenCalled();
  expect(chooseEffort).not.toHaveBeenCalled();
  await user.click(screen.getByRole("button", { name: "AI model" }));
  await user.click(await screen.findByText("Model B"));
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  expect(chooseModel).toHaveBeenCalledExactlyOnceWith("b");
  expect(chooseEffort).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "AI model" }).textContent).toContain("Model B");
  await user.click(screen.getByRole("button", { name: "Reasoning effort" }));
  await user.click(await screen.findByText("Low"));
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  expect(chooseEffort).toHaveBeenCalledExactlyOnceWith("low");
  expect(chooseModel).toHaveBeenCalledOnce();
  expect((screen.getByRole("textbox", { name: "Draft" }) as HTMLInputElement).value).toBe("Keep this draft");
});

test("choosing the current model still reports the explicit choice and closes its menu", async () => {
  const user = userEvent.setup(), change = vi.fn();
  render(<ModelPicker groups={groups} model={models[0]} value="a" disabled={false} onChange={change} />);
  await user.click(screen.getByRole("button", { name: "AI model" }));
  await user.click(await screen.findByRole("menuitemradio", { name: "Model A" }));
  expect(change).toHaveBeenCalledExactlyOnceWith("a");
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
});

test("focus inside the menu keeps it open; clicking the draft dismisses it without selection", async () => {
  const user = userEvent.setup(), change = vi.fn();
  render(<><input aria-label="Draft" defaultValue="Retained" />
    <ModelPicker groups={groups} model={models[0]} value="a" disabled={false} onChange={change} /></>);
  await user.click(screen.getByRole("button", { name: "AI model" }));
  (await screen.findByRole("menuitemradio", { name: "Model B" })).focus();
  expect(screen.getByRole("menu")).toBeTruthy();
  await user.click(screen.getByRole("textbox", { name: "Draft" }));
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  expect(change).not.toHaveBeenCalled();
  expect((screen.getByRole("textbox", { name: "Draft" }) as HTMLInputElement).value).toBe("Retained");
});

test("picker Escape returns focus to its own trigger and disabled choices stay unchanged", async () => {
  const user = userEvent.setup(), change = vi.fn();
  const { rerender } = render(<ModelPicker groups={groups} model={models[0]} value="a" disabled={false} onChange={change} />);
  const trigger = screen.getByRole("button", { name: "AI model" });
  // jsdom lacks the native summary tab stop provided by browsers.
  if (trigger.tagName === "SUMMARY") trigger.tabIndex = 0;
  await user.click(trigger);
  (await screen.findByText("Model B")).closest("button")!.focus();
  await user.keyboard("{Escape}");
  expect(document.activeElement).toBe(trigger);
  expect(change).not.toHaveBeenCalled();
  rerender(<ModelPicker groups={groups} model={models[0]} value="a" disabled onChange={change} />);
  await user.click(trigger);
  expect(change).not.toHaveBeenCalled();
  expect((trigger as HTMLButtonElement).disabled || trigger.getAttribute("aria-disabled") === "true").toBe(true);
});

test("confirmation keeps cancel focus, Escape, confirmation and the opener draft", async () => {
  const user = userEvent.setup(), confirm = vi.fn(), cancel = vi.fn();
  function Harness() {
    const [open, setOpen] = useState(false);
    return <><input aria-label="Draft" defaultValue="Retain my input" />
      <button onClick={() => setOpen(true)}>Remove</button>
      <ConfirmDialog open={open} title="Remove item?" message="This cannot be undone" confirmLabel="Remove item" cancelLabel="Cancel"
        onCancel={() => { cancel(); setOpen(false); }} onConfirm={() => { confirm(); setOpen(false); }} />
    </>;
  }
  render(<Harness />);
  const opener = screen.getByRole("button", { name: "Remove" });
  await user.click(opener);
  await screen.findByRole("dialog", { name: "Remove item?" });
  await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("button", { name: "Cancel" })));
  await user.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await waitFor(() => expect(document.activeElement).toBe(opener));
  expect(cancel).toHaveBeenCalledOnce();
  expect(confirm).not.toHaveBeenCalled();
  await user.click(opener);
  await user.click(await screen.findByRole("button", { name: "Remove item" }));
  expect(confirm).toHaveBeenCalledOnce();
  expect((screen.getByRole("textbox", { name: "Draft" }) as HTMLInputElement).value).toBe("Retain my input");
});

test("a busy confirmation cannot dismiss through Escape or backdrop", async () => {
  const user = userEvent.setup(), cancel = vi.fn();
  render(<ConfirmDialog open busy title="Saving" message="Please wait" confirmLabel="Confirm" cancelLabel="Cancel" onCancel={cancel} onConfirm={() => {}} />);
  await user.keyboard("{Escape}");
  await user.click(screen.getByRole("dialog").parentElement!);
  expect(cancel).not.toHaveBeenCalled();
  expect(screen.getByRole("dialog", { name: "Saving" })).toBeTruthy();
  expect((screen.getByRole("button", { name: "Cancel" }) as HTMLButtonElement).disabled).toBe(true);
});
