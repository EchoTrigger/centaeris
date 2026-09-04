import { useCallback, type RefObject } from "react";
import type { ActiveStreamState, ChatMessage } from "./types";

type SetMessages = (
  updater: ChatMessage[] | ((previous: ChatMessage[]) => ChatMessage[]),
) => void;

type UseDurableTurnMessageIdsOptions = {
  setMessages: SetMessages;
  getActiveStream: () => ActiveStreamState | null;
  stoppedAssistantMessageIdsRef: RefObject<Set<string>>;
};

export const useDurableTurnMessageIds = ({
  setMessages,
  getActiveStream,
  stoppedAssistantMessageIdsRef,
}: UseDurableTurnMessageIdsOptions) => {
  const applyDurableTurnMessageIds = useCallback(
    (
      temporaryUserMessageId: string,
      temporaryAssistantMessageId: string,
      turnIdValue: string | undefined,
    ): { userMessageId: string; assistantMessageId: string } => {
      const turnId = typeof turnIdValue === "string" ? turnIdValue.trim() : "";
      if (!turnId) {
        return {
          userMessageId: temporaryUserMessageId,
          assistantMessageId: temporaryAssistantMessageId,
        };
      }
      const userMessageId = `msg:user:${turnId}`;
      const assistantMessageId = `msg:assistant:${turnId}`;
      if (
        userMessageId === temporaryUserMessageId &&
        assistantMessageId === temporaryAssistantMessageId
      ) {
        return { userMessageId, assistantMessageId };
      }
      setMessages((previous) =>
        previous.map((item) => {
          if (item.id === temporaryUserMessageId && item.role === "user") {
            return {
              ...item,
              id: userMessageId,
            };
          }
          if (
            item.id === temporaryAssistantMessageId &&
            item.role === "assistant"
          ) {
            return {
              ...item,
              id: assistantMessageId,
            };
          }
          return item;
        }),
      );
      const activeStream = getActiveStream();
      if (activeStream?.assistantMessageId === temporaryAssistantMessageId) {
        activeStream.assistantMessageId = assistantMessageId;
      }
      if (
        stoppedAssistantMessageIdsRef.current.delete(
          temporaryAssistantMessageId,
        )
      ) {
        stoppedAssistantMessageIdsRef.current.add(assistantMessageId);
      }
      return { userMessageId, assistantMessageId };
    },
    [getActiveStream, setMessages, stoppedAssistantMessageIdsRef],
  );

  return { applyDurableTurnMessageIds };
};
