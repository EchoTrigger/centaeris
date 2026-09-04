import assert from "node:assert/strict";
import { test } from "vitest";
import { useChatViewStore } from "../src/components/chat/chatViewStore.ts";

test("projects chat view state without unnecessary identity changes", () => {
  const userMessage = {
    id: "msg:user:1",
    role: "user",
    text: "hello",
    timestamp: 1,
  };
  const assistantMessage = {
    id: "msg:assistant:1",
    role: "assistant",
    turn: {
      id: "turn-1",
      chunks: [
        {
          id: "chunk-task-1",
          kind: "task",
          task: {
            id: "task-1",
            title: "read",
            summary: "read files",
            status: "done",
            provider: "tool",
          },
        },
        {
          id: "chunk-task-2",
          kind: "task",
          task: {
            id: "task-2",
            title: "Build",
            summary: "building",
            status: "running",
            provider: "tool",
          },
        },
      ],
      finalAnswer: "",
      isStreaming: true,
    },
  };
  useChatViewStore.getState().replaceMessages([userMessage, assistantMessage]);
  const firstState = useChatViewStore.getState();
  assert.deepEqual(firstState.messageIds, ["msg:user:1", "msg:assistant:1"]);
  assert.equal(firstState.messageById["msg:user:1"], userMessage);
  assert.equal(firstState.messageById["msg:assistant:1"], assistantMessage);
  assert.equal(firstState.turnIdByMessageId["msg:assistant:1"], "turn-1");
  assert.equal(firstState.turnById["turn-1"], assistantMessage.turn);
  assert.equal(
    firstState.taskById["task-1"],
    assistantMessage.turn.chunks[0].task,
  );
  assert.equal(
    firstState.taskById["task-2"],
    assistantMessage.turn.chunks[1].task,
  );

  const textOnlyAssistantMessage = {
    ...assistantMessage,
    turn: {
      ...assistantMessage.turn,
      finalAnswer: "streaming",
    },
  };
  useChatViewStore
    .getState()
    .updateAssistantMessages([textOnlyAssistantMessage]);
  const textOnlyState = useChatViewStore.getState();
  assert.equal(textOnlyState.messageIds, firstState.messageIds);
  assert.equal(textOnlyState.taskById, firstState.taskById);
  assert.equal(textOnlyState.chunkIdsByTurnId, firstState.chunkIdsByTurnId);
  assert.equal(textOnlyState.turnById["turn-1"], textOnlyAssistantMessage.turn);

  const unchangedTask = assistantMessage.turn.chunks[0].task;
  const updatedTask = {
    ...assistantMessage.turn.chunks[1].task,
    status: "done",
    summary: "built",
  };
  const updatedAssistantMessage = {
    ...assistantMessage,
    turn: {
      ...assistantMessage.turn,
      finalAnswer: "done",
      chunks: [
        assistantMessage.turn.chunks[0],
        {
          ...assistantMessage.turn.chunks[1],
          task: updatedTask,
        },
      ],
    },
  };
  useChatViewStore
    .getState()
    .updateAssistantMessages([updatedAssistantMessage]);
  const secondState = useChatViewStore.getState();
  assert.equal(secondState.messageById["msg:user:1"], userMessage);
  assert.notEqual(secondState.messageById["msg:assistant:1"], assistantMessage);
  assert.equal(
    secondState.messageById["msg:assistant:1"],
    updatedAssistantMessage,
  );
  assert.equal(secondState.taskById["task-1"], unchangedTask);
  assert.equal(secondState.taskById["task-2"], updatedTask);

  useChatViewStore.getState().clear();
  assert.deepEqual(useChatViewStore.getState().messageIds, []);
});
