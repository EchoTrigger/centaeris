import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import type { AgentStreamPayload } from "../src/lib/chatBridge";
import {
  useAgentStreamController,
  type AgentStreamController,
} from "../src/components/chat/useAgentStreamController";
import type { AgentStreamConnection } from "../src/components/chat/useAgentStreamConnection";
import type { AssistantTurnUpdateQueue } from "../src/components/chat/useAssistantTurnUpdateQueue";
import type { ActiveStreamState } from "../src/components/chat/types";

const bridge = vi.hoisted(() => ({
  openAgentStream: vi.fn(),
}));

vi.mock("../src/lib/chatBridge", () => ({
  openAgentStream: bridge.openAgentStream,
}));

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

test("starting the same assistant message and run twice opens one stream", async () => {
  const close = vi.fn();
  bridge.openAgentStream.mockReset();
  bridge.openAgentStream.mockReturnValue({ close });
  let activeStream: ActiveStreamState | null = null;
  const connection: AgentStreamConnection = {
    getActiveStream: () => activeStream,
    isActiveStream: ({ assistantMessageId, agentRunId }) =>
      activeStream?.assistantMessageId === assistantMessageId &&
      activeStream.agentRunId === agentRunId,
    isStreaming: false,
    setIsStreaming: vi.fn(),
    setActiveStream: (stream) => {
      activeStream = stream;
    },
    markStreamOpen: vi.fn(),
    closeActiveStream: vi.fn(() => {
      activeStream?.close();
      activeStream = null;
    }),
    closeStreamForMessage: vi.fn(),
  };
  const turnUpdates: AssistantTurnUpdateQueue = {
    updateAssistantTurn: vi.fn(),
    appendAssistantTextDelta: vi.fn(),
    flushAssistantTurnUpdates: vi.fn(),
  };
  const onAgentRunningChange = vi.fn();
  let controller: AgentStreamController | null = null;
  const Harness = () => {
    controller = useAgentStreamController({
      connection,
      turnUpdates,
      context: {
        markContextCompacting: vi.fn(),
        refreshContextUsage: vi.fn(async () => {}),
      },
      question: {
        setPendingQuestion: vi.fn(),
        setPendingQuestionError: vi.fn(),
      },
      sessionOutcome: {
        pendingResolvedSessionRef: { current: null },
        preserveResolvedSessionIdRef: { current: null },
        onSessionResolved: undefined,
        onAgentRunningChange,
        onSessionCompleted: undefined,
      },
      replay: {
        rememberReplayPayloads: vi.fn(
          (_payloads: readonly AgentStreamPayload[]) => {},
        ),
        visibleActiveReplayRef: { current: null },
      },
    });
    return null;
  };
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(<Harness />);
  });
  const getController = (): AgentStreamController => {
    if (!controller) {
      throw new Error("agent stream controller did not initialize");
    }
    return controller;
  };
  const activeController = getController();

  activeController.startStreamForAssistant(
    "assistant-one",
    "session-one",
    "run-one",
  );
  activeController.startStreamForAssistant(
    "assistant-one",
    "session-one",
    "run-one",
  );

  expect(bridge.openAgentStream).toHaveBeenCalledTimes(1);
  expect(onAgentRunningChange).toHaveBeenCalledTimes(1);
  expect(onAgentRunningChange).toHaveBeenCalledWith("session-one", true);
  expect(connection.closeActiveStream).toHaveBeenCalledTimes(1);
  expect(turnUpdates.updateAssistantTurn).toHaveBeenCalledTimes(2);

  await act(async () => renderer?.unmount());
});
