import { useCallback, useEffect, useRef, type RefObject } from "react";
import type { AssistantExecutionTurn, ChatMessage } from "./types";

export type AssistantTurnUpdater = (
  turn: AssistantExecutionTurn,
) => AssistantExecutionTurn;

type PendingAssistantTurnUpdate =
  | { kind: "textDelta"; delta: string }
  | { kind: "update"; updater: AssistantTurnUpdater };

type AssistantChatMessage = Extract<ChatMessage, { role: "assistant" }>;

export type AssistantTurnCommitOptions = {
  assistantMessages?: AssistantChatMessage[];
  refreshMeta?: boolean;
};

export type AssistantTurnUpdateQueue = {
  updateAssistantTurn: (
    messageId: string,
    updater: AssistantTurnUpdater,
  ) => void;
  appendAssistantTextDelta: (messageId: string, delta: string) => void;
  flushAssistantTurnUpdates: () => void;
};

type AssistantTurnUpdateQueueOptions = {
  messagesRef: RefObject<ChatMessage[]>;
  messageIndexByIdRef: RefObject<Map<string, number>>;
  commitMessagesToView: (
    nextMessages: ChatMessage[],
    options?: AssistantTurnCommitOptions,
  ) => void;
  flushAssistantTurnUpdatesRef: RefObject<() => void>;
};

export const useAssistantTurnUpdateQueue = ({
  messagesRef,
  messageIndexByIdRef,
  commitMessagesToView,
  flushAssistantTurnUpdatesRef,
}: AssistantTurnUpdateQueueOptions): AssistantTurnUpdateQueue => {
  const pendingUpdatesRef = useRef<
    Map<string, PendingAssistantTurnUpdate[]>
  >(new Map());
  const updateFrameRef = useRef<number | null>(null);

  const flushAssistantTurnUpdates = useCallback(() => {
    if (updateFrameRef.current !== null) {
      window.cancelAnimationFrame(updateFrameRef.current);
      updateFrameRef.current = null;
    }
    if (pendingUpdatesRef.current.size === 0) {
      return;
    }
    const pendingUpdates = pendingUpdatesRef.current;
    pendingUpdatesRef.current = new Map();
    const previous = messagesRef.current;
    let changed = false;
    let next: ChatMessage[] | null = null;
    let refreshMeta = false;
    const changedAssistantMessages: AssistantChatMessage[] = [];
    for (const [messageId, updaters] of pendingUpdates.entries()) {
      if (updaters.length === 0) {
        continue;
      }
      const messageIndex = messageIndexByIdRef.current.get(messageId);
      if (messageIndex === undefined) {
        continue;
      }
      const item = previous[messageIndex];
      if (!item || item.role !== "assistant" || item.id !== messageId) {
        continue;
      }
      let nextTurn = item.turn;
      let pendingTextDeltas: string[] = [];
      const flushTextDeltas = () => {
        if (pendingTextDeltas.length === 0) {
          return;
        }
        nextTurn = {
          ...nextTurn,
          finalAnswer: `${nextTurn.finalAnswer}${pendingTextDeltas.join("")}`,
          activity: null,
        };
        pendingTextDeltas = [];
      };
      for (const update of updaters) {
        if (update.kind === "textDelta") {
          pendingTextDeltas.push(update.delta);
          continue;
        }
        flushTextDeltas();
        nextTurn = update.updater(nextTurn);
        refreshMeta = true;
      }
      flushTextDeltas();
      if (nextTurn === item.turn) {
        continue;
      }
      if (!next) {
        next = [...previous];
      }
      changed = true;
      const nextMessage: AssistantChatMessage = {
        ...item,
        turn: nextTurn,
      };
      next[messageIndex] = nextMessage;
      changedAssistantMessages.push(nextMessage);
    }
    if (!changed) {
      return;
    }
    commitMessagesToView(next ?? previous, {
      assistantMessages: changedAssistantMessages,
      refreshMeta,
    });
  }, [commitMessagesToView, messageIndexByIdRef, messagesRef]);
  flushAssistantTurnUpdatesRef.current = flushAssistantTurnUpdates;

  const scheduleAssistantTurnFlush = useCallback(() => {
    if (updateFrameRef.current !== null) {
      return;
    }
    updateFrameRef.current = window.requestAnimationFrame(() => {
      updateFrameRef.current = null;
      flushAssistantTurnUpdates();
    });
  }, [flushAssistantTurnUpdates]);

  const updateAssistantTurn = useCallback(
    (messageId: string, updater: AssistantTurnUpdater) => {
      const normalizedMessageId = messageId.trim();
      if (!normalizedMessageId) {
        throw new Error("assistant message id is required for turn update");
      }
      const pendingUpdates = pendingUpdatesRef.current;
      const updaters = pendingUpdates.get(normalizedMessageId);
      const pendingUpdate: PendingAssistantTurnUpdate = {
        kind: "update",
        updater,
      };
      if (updaters) {
        updaters.push(pendingUpdate);
      } else {
        pendingUpdates.set(normalizedMessageId, [pendingUpdate]);
      }
      scheduleAssistantTurnFlush();
    },
    [scheduleAssistantTurnFlush],
  );

  const appendAssistantTextDelta = useCallback(
    (messageId: string, delta: string) => {
      if (!delta) {
        return;
      }
      const normalizedMessageId = messageId.trim();
      if (!normalizedMessageId) {
        throw new Error("assistant message id is required for text delta");
      }
      const pendingUpdates = pendingUpdatesRef.current;
      const update: PendingAssistantTurnUpdate = { kind: "textDelta", delta };
      const updates = pendingUpdates.get(normalizedMessageId);
      if (updates) {
        updates.push(update);
      } else {
        pendingUpdates.set(normalizedMessageId, [update]);
      }
      scheduleAssistantTurnFlush();
    },
    [scheduleAssistantTurnFlush],
  );

  useEffect(
    () => () => {
      if (updateFrameRef.current !== null) {
        window.cancelAnimationFrame(updateFrameRef.current);
        updateFrameRef.current = null;
      }
      pendingUpdatesRef.current.clear();
    },
    [],
  );

  return {
    updateAssistantTurn,
    appendAssistantTextDelta,
    flushAssistantTurnUpdates,
  };
};
