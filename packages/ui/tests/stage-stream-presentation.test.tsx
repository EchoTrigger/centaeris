// @vitest-environment jsdom
import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { AgentResultStream } from "../src/components/chat/AgentResultStream";
import { applySessionEventToAssistantTurn } from "../src/components/chat/chatTranscriptRestore";
import type { AssistantExecutionTurn } from "../src/components/chat/types";
import type { SessionEvent } from "../src/lib/chatBridge";

beforeEach(() => {
  vi.stubGlobal("matchMedia", () => ({ matches: false }));
  vi.spyOn(document, "hidden", "get").mockReturnValue(false);
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function events() {
  let sequence = 0;
  return (
    type: string,
    payload: Record<string, unknown>,
    extra: Partial<SessionEvent> = {},
  ): SessionEvent => ({
    id: "stage-event-" + ++sequence,
    type,
    sessionId: "stage-session",
    turnId: "model-turn",
    taskId: "stage-run",
    visibility: "user",
    status: "running",
    payload,
    ...extra,
  });
}
const pending = (): AssistantExecutionTurn => ({
  id: "stage-turn",
  agentRunId: "stage-run",
  chunks: [],
  finalAnswer: "",
  isStreaming: true,
});

for (const boundary of ["Status", "ToolCall"])
  test("stage presentation survives " + boundary + " while tools appear immediately", async () => {
    const event = events();
    const target = "Checking the implementation carefully. ".repeat(18).trim();
    const source = boundary === "Status" ? "\n\n" + target + "\n" : target;
    let turn = pending();
    const view = render(<AgentResultStream turn={turn} />);
    turn = applySessionEventToAssistantTurn(
      turn,
      event("ModelSnapshot", { revision: 1, text: source, reasoning: null }),
    );
    view.rerender(<AgentResultStream turn={turn} />);
    await waitFor(() =>
      expect(
        view.container.querySelector(".markdown-content")?.textContent!.length,
      ).toBeGreaterThan(2),
    );
    const reader = view.container.querySelector(".markdown-content")!;
    expect(reader.textContent!.length).toBeLessThan(target.length);
    const before = reader.textContent;
    if (boundary === "Status")
      turn = applySessionEventToAssistantTurn(
        turn,
        event("Status", { stage: "model_process_summary", message: target }),
      );
    turn = applySessionEventToAssistantTurn(
      turn,
      event(
        "ToolCall",
        {
          callId: "inspect",
          summary: "Inspect project",
          normalizedInput: { command: "echo check" },
        },
        { toolName: "exec_command" },
      ),
    );
    view.rerender(<AgentResultStream turn={turn} />);
    const archived = view.container.querySelector(".agentProcessSectionHeading .markdown-content");
    expect(archived).toBe(reader);
    expect(archived!.textContent).toBe(before);
    expect(view.getByText("Running an external tool")).toBeTruthy();

    // Equal wording in a later model reply must have its own presentation.
    turn = applySessionEventToAssistantTurn(
      turn,
      event("ModelSnapshot", { revision: 2, text: target, reasoning: null }),
    );
    view.rerender(<AgentResultStream turn={turn} />);
    await waitFor(() =>
      expect(
        view.container.querySelector(".agentAssistantAnswer .markdown-content"),
      ).not.toBeNull(),
    );
    const nextReader = view.container.querySelector(".agentAssistantAnswer .markdown-content")!;
    expect(nextReader).not.toBe(reader);
    expect(reader.textContent!.length).toBeLessThan(target.length);
    expect(nextReader.textContent!.length).toBeLessThan(target.length);
    turn = applySessionEventToAssistantTurn(turn, event("Final", { content: target }));
    turn = applySessionEventToAssistantTurn(
      turn,
      event("AgentRunCompleted", {}, { status: "done" }),
    );
    view.rerender(<AgentResultStream turn={turn} />);
    expect(view.container.querySelector(".agentAssistantAnswer .markdown-content")).toBe(
      nextReader,
    );
    expect(nextReader.textContent!.length).toBeLessThan(target.length);
    await waitFor(
      () => {
        expect(reader.textContent).toBe(target);
        expect(nextReader.textContent).toBe(target);
      },
      { timeout: 3000 },
    );
  });

test("opened history shows complete process text without replaying presentation", () => {
  const target = "Previously completed process text. ".repeat(30).trim();
  const view = render(
    <AgentResultStream
      turn={{
        ...pending(),
        isStreaming: false,
        chunks: [{ id: "historic-stage", kind: "narrative", text: target }],
        finalAnswer: "Historical answer",
      }}
    />,
  );
  expect(view.container.querySelector(".agentProcessSectionHeading")?.textContent).toBe(target);
  expect(view.container.querySelector(".agentAssistantAnswer")?.textContent).toBe(
    "Historical answer",
  );
});

test("repeated stage wording retains separate model text blocks", () => {
  const event = events();
  let turn = pending();
  for (let revision = 1; revision <= 2; revision++) {
    turn = applySessionEventToAssistantTurn(
      turn,
      event("ModelSnapshot", { revision, text: "Repeated stage", reasoning: null }),
    );
    turn = applySessionEventToAssistantTurn(
      turn,
      event("Status", { stage: "model_process_summary", message: "Repeated stage" }),
    );
  }
  const text = turn.chunks.filter((chunk) => chunk.kind === "narrative");
  expect(text.map((chunk) => chunk.text)).toEqual(["Repeated stage", "Repeated stage"]);
  expect(new Set(text.map((chunk) => chunk.id)).size).toBe(2);
});

test("a model correction replaces the current text immediately and archives without replay", async () => {
  const event = events();
  let turn = applySessionEventToAssistantTurn(
    pending(),
    event("ModelSnapshot", { revision: 1, text: "Draft text. ".repeat(80), reasoning: null }),
  );
  const view = render(<AgentResultStream turn={turn} />);
  const reader = view.container.querySelector(".markdown-content")!;
  const corrected = "Corrected authoritative text.";
  turn = applySessionEventToAssistantTurn(turn, event("ModelTextReplace", { content: corrected }));
  view.rerender(<AgentResultStream turn={turn} />);
  expect(reader.textContent).toBe(corrected);
  turn = applySessionEventToAssistantTurn(
    turn,
    event("Status", { stage: "model_process_summary", message: corrected }),
  );
  view.rerender(<AgentResultStream turn={turn} />);
  expect(view.container.querySelector(".agentProcessSectionHeading .markdown-content")).toBe(
    reader,
  );
  expect(reader.textContent).toBe(corrected);
});

test("a bounded process summary cannot duplicate or replace the streamed model block", () => {
  const event = events();
  const source = "First paragraph.\n\nSecond paragraph with details. ".repeat(40);
  let turn = applySessionEventToAssistantTurn(
    pending(),
    event("ModelSnapshot", { revision: 1, text: source, reasoning: null }),
  );
  turn = applySessionEventToAssistantTurn(
    turn,
    event("Status", {
      stage: "model_process_summary",
      message: "First paragraph. Second paragraph with details...",
    }),
  );
  const text = turn.chunks.filter((chunk) => chunk.kind === "narrative");
  expect(text).toHaveLength(1);
  expect(text[0].text).toBe(source);
});
