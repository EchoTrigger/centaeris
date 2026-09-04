import { create } from "zustand";
import type {
  AssistantExecutionTurn,
  ChatMessage,
  SubagentResult,
  TaskResult,
} from "./types";

type AssistantChatMessage = Extract<ChatMessage, { role: "assistant" }>;

type ChatViewState = {
  messageIds: string[];
  messageById: Record<string, ChatMessage>;
  turnById: Record<string, AssistantExecutionTurn>;
  turnIdByMessageId: Record<string, string>;
  taskById: Record<string, TaskResult>;
  subagentById: Record<string, SubagentResult>;
  chunkIdsByTurnId: Record<string, string[]>;
  replaceMessages: (messages: readonly ChatMessage[]) => void;
  updateAssistantMessages: (messages: readonly AssistantChatMessage[]) => void;
  clear: () => void;
};

type ChatViewIndexes = Pick<
  ChatViewState,
  | "messageIds"
  | "messageById"
  | "turnById"
  | "turnIdByMessageId"
  | "taskById"
  | "subagentById"
  | "chunkIdsByTurnId"
>;

const normalizeMessages = (
  messages: readonly ChatMessage[],
  previous: ChatViewState,
): ChatViewIndexes => {
  const messageById: Record<string, ChatMessage> = {};
  const turnById: Record<string, AssistantExecutionTurn> = {};
  const turnIdByMessageId: Record<string, string> = {};
  const taskById: Record<string, TaskResult> = {};
  const subagentById: Record<string, SubagentResult> = {};
  const chunkIdsByTurnId: Record<string, string[]> = {};

  for (const message of messages) {
    const previousMessage = previous.messageById[message.id];
    messageById[message.id] =
      previousMessage === message ? previousMessage : message;
    if (message.role !== "assistant") {
      continue;
    }
    const turn = message.turn;
    turnById[turn.id] = previous.turnById[turn.id] === turn
      ? previous.turnById[turn.id]
      : turn;
    turnIdByMessageId[message.id] = turn.id;
    chunkIdsByTurnId[turn.id] = turn.chunks.map((chunk) => chunk.id);
    for (const chunk of turn.chunks) {
      if (chunk.kind === "task") {
        const task = chunk.task;
        taskById[task.id] =
          previous.taskById[task.id] === task ? previous.taskById[task.id] : task;
      }
      if (chunk.kind === "subagent") {
        const subagent = chunk.subagent;
        subagentById[subagent.id] =
          previous.subagentById[subagent.id] === subagent
            ? previous.subagentById[subagent.id]
            : subagent;
      }
    }
  }
  return {
    messageIds: messages.map((message) => message.id),
    messageById,
    turnById,
    turnIdByMessageId,
    taskById,
    subagentById,
    chunkIdsByTurnId,
  };
};

export const useChatViewStore = create<ChatViewState>((set) => ({
  messageIds: [],
  messageById: {},
  turnById: {},
  turnIdByMessageId: {},
  taskById: {},
  subagentById: {},
  chunkIdsByTurnId: {},
  replaceMessages: (messages) =>
    set((state) => normalizeMessages(messages, state)),
  updateAssistantMessages: (messages) =>
    set((state) => {
      if (messages.length === 0) {
        return state;
      }
      let changed = false;
      let messageById = state.messageById;
      let turnById = state.turnById;
      let turnIdByMessageId = state.turnIdByMessageId;
      let taskById = state.taskById;
      let subagentById = state.subagentById;
      let chunkIdsByTurnId = state.chunkIdsByTurnId;
      for (const message of messages) {
        const previousMessage = state.messageById[message.id];
        if (previousMessage === message) {
          continue;
        }
        if (!changed) {
          changed = true;
          messageById = { ...messageById };
          turnById = { ...turnById };
          turnIdByMessageId = { ...turnIdByMessageId };
        }
        messageById[message.id] = message;
        turnById[message.turn.id] = message.turn;
        turnIdByMessageId[message.id] = message.turn.id;
        const previousTurn =
          previousMessage?.role === "assistant" ? previousMessage.turn : undefined;
        if (previousTurn?.chunks === message.turn.chunks) {
          continue;
        }
        if (taskById === state.taskById) {
          taskById = { ...taskById };
          subagentById = { ...subagentById };
          chunkIdsByTurnId = { ...chunkIdsByTurnId };
        }
        chunkIdsByTurnId[message.turn.id] = message.turn.chunks.map(
          (chunk) => chunk.id,
        );
        for (const chunk of message.turn.chunks) {
          if (chunk.kind === "task") {
            taskById[chunk.task.id] = chunk.task;
          } else if (chunk.kind === "subagent") {
            subagentById[chunk.subagent.id] = chunk.subagent;
          }
        }
      }
      return changed
        ? {
            messageById,
            turnById,
            turnIdByMessageId,
            taskById,
            subagentById,
            chunkIdsByTurnId,
          }
        : state;
    }),
  clear: () =>
    set({
      messageIds: [],
      messageById: {},
      turnById: {},
      turnIdByMessageId: {},
      taskById: {},
      subagentById: {},
      chunkIdsByTurnId: {},
    }),
}));

export const selectChatMessageIds = (state: ChatViewState): string[] =>
  state.messageIds;

export const selectChatMessageById =
  (messageId: string) =>
    (state: ChatViewState): ChatMessage | undefined =>
      state.messageById[messageId];

export const selectChatMessageRoleById =
  (messageId: string) =>
    (state: ChatViewState): ChatMessage["role"] | undefined =>
      state.messageById[messageId]?.role;

export const selectChatTurnByMessageId =
  (messageId: string) =>
    (state: ChatViewState): AssistantExecutionTurn | undefined => {
      const turnId = state.turnIdByMessageId[messageId];
      return turnId ? state.turnById[turnId] : undefined;
    };

export const selectChatTaskById =
  (taskId: string) =>
    (state: ChatViewState): TaskResult | undefined =>
      state.taskById[taskId];
