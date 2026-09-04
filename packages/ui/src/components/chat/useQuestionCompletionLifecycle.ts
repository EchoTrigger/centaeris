import { useCallback, useEffect, useRef } from "react";
import { answerAgentQuestion, type AgentStreamPayload } from "../../lib/chatBridge";
import { appendNarrativeChunk } from "./chatAreaModel";
import { buildPendingTurn } from "./chatRuntimeModel";
import type {
  AssistantExecutionTurn,
  ChatMessage,
  PendingQuestionState,
} from "./types";

type SetPendingQuestion = (
  updater:
    | PendingQuestionState
    | null
    | ((previous: PendingQuestionState | null) => PendingQuestionState | null),
) => void;

type SetMessages = (
  updater: ChatMessage[] | ((previous: ChatMessage[]) => ChatMessage[]),
) => void;

type StartStreamForAssistant = (
  assistantMessageId: string,
  sessionId: string,
  agentRunId: string,
  seedPayloads?: AgentStreamPayload[],
) => void;

type UpdateAssistantTurn = (
  messageId: string,
  updater: (turn: AssistantExecutionTurn) => AssistantExecutionTurn,
) => void;

type UseQuestionCompletionLifecycleOptions = {
  sessionId: string;
  autoContinueAfterResumeWait: boolean | undefined;
  pendingQuestion: PendingQuestionState | null;
  setPendingQuestion: SetPendingQuestion;
  setPendingQuestionError: (message: string) => void;
  setMessages: SetMessages;
  startStreamForAssistant: StartStreamForAssistant;
  refreshContextUsage: (sessionId: string) => Promise<void>;
  updateAssistantTurn: UpdateAssistantTurn;
  onAgentRunningChange?: (sessionId: string, isRunning: boolean) => void;
};

export const useQuestionCompletionLifecycle = ({
  sessionId,
  autoContinueAfterResumeWait,
  pendingQuestion,
  setPendingQuestion,
  setPendingQuestionError,
  setMessages,
  startStreamForAssistant,
  refreshContextUsage,
  updateAssistantTurn,
  onAgentRunningChange,
}: UseQuestionCompletionLifecycleOptions) => {
  const normalizedSessionId = sessionId.trim();
  const activeSessionIdRef = useRef(normalizedSessionId);
  const submissionEpochRef = useRef(0);
  const submissionInFlightRef = useRef(false);

  useEffect(() => {
    activeSessionIdRef.current = normalizedSessionId;
    submissionEpochRef.current += 1;
    submissionInFlightRef.current = false;
    return () => {
      submissionEpochRef.current += 1;
      submissionInFlightRef.current = false;
    };
  }, [normalizedSessionId]);

  const handleQuestionOptionToggle = useCallback(
    (option: string) => {
      setPendingQuestion((previous) => {
        if (!previous || previous.submitting) {
          return previous;
        }
        if (!previous.request.multiSelect) {
          return {
            ...previous,
            selectedOptions: [option],
          };
        }
        const hasOption = previous.selectedOptions.includes(option);
        return {
          ...previous,
          selectedOptions: hasOption
            ? previous.selectedOptions.filter((item) => item !== option)
            : [...previous.selectedOptions, option],
        };
      });
      setPendingQuestionError("");
    },
    [setPendingQuestion, setPendingQuestionError],
  );

  const handleQuestionTextChange = useCallback(
    (value: string) => {
      setPendingQuestion((previous) => {
        if (!previous || previous.submitting) {
          return previous;
        }
        return {
          ...previous,
          answerText: value,
        };
      });
      setPendingQuestionError("");
    },
    [setPendingQuestion, setPendingQuestionError],
  );

  const handleQuestionSubmit = useCallback(async () => {
    if (
      !pendingQuestion ||
      pendingQuestion.submitting ||
      !normalizedSessionId ||
      submissionInFlightRef.current
    ) {
      return;
    }

    const answerText = pendingQuestion.answerText.trim();
    const answers = pendingQuestion.selectedOptions;
    if (
      pendingQuestion.request.required &&
      answers.length === 0 &&
      !answerText
    ) {
      setPendingQuestionError("请至少提供一个回答。");
      return;
    }

    submissionInFlightRef.current = true;
    const submissionEpoch = submissionEpochRef.current;
    const ownsCurrentSession = (): boolean =>
      submissionEpochRef.current === submissionEpoch &&
      activeSessionIdRef.current === normalizedSessionId;

    setPendingQuestionError("");
    setPendingQuestion((previous) => {
      if (!previous) {
        return previous;
      }
      return {
        ...previous,
        submitting: true,
      };
    });

    const now = Date.now();
    const userSummary =
      answers.length > 0
        ? `回答：${answers.join("；")}${answerText ? `；${answerText}` : ""}`
        : `回答：${answerText || "已提交"}`;
    const userMessage: ChatMessage = {
      id: `user-question-${now}`,
      role: "user",
      text: userSummary,
      timestamp: now,
    };
    const assistantMessage: ChatMessage = {
      id: `assistant-question-${now}`,
      role: "assistant",
      turn: buildPendingTurn(),
    };
    setMessages((previous) => [...previous, userMessage, assistantMessage]);

    try {
      const response = await answerAgentQuestion({
        sessionId: normalizedSessionId,
        questionId: pendingQuestion.request.id,
        answers: answers.length > 0 ? answers : undefined,
        answerText: answerText || undefined,
        autoContinueAfterResumeWait,
      });
      if (!ownsCurrentSession()) {
        return;
      }
      const responseSessionId =
        typeof response.sessionId === "string" &&
        response.sessionId.trim().length > 0
          ? response.sessionId.trim()
          : normalizedSessionId;
      const responseAgentRunId =
        typeof response.agentRunId === "string" &&
        response.agentRunId.trim().length > 0
          ? response.agentRunId.trim()
          : "";
      if (!responseAgentRunId) {
        throw new Error("missing agentRunId");
      }
      setPendingQuestion(null);
      startStreamForAssistant(
        assistantMessage.id,
        responseSessionId,
        responseAgentRunId,
        [],
      );
      void refreshContextUsage(responseSessionId);
    } catch {
      if (!ownsCurrentSession()) {
        return;
      }
      onAgentRunningChange?.(normalizedSessionId, false);
      updateAssistantTurn(assistantMessage.id, (turn) => {
        const withError = appendNarrativeChunk(
          turn,
          "回答提交失败，请稍后重试。",
          "error",
        );
        return {
          ...withError,
          isStreaming: false,
          activity: undefined,
        };
      });
      setPendingQuestion((previous) => {
        if (!previous) {
          return previous;
        }
        return {
          ...previous,
          submitting: false,
        };
      });
    } finally {
      if (ownsCurrentSession()) {
        submissionInFlightRef.current = false;
      }
    }
  }, [
    autoContinueAfterResumeWait,
    normalizedSessionId,
    onAgentRunningChange,
    pendingQuestion,
    refreshContextUsage,
    setMessages,
    setPendingQuestion,
    setPendingQuestionError,
    startStreamForAssistant,
    updateAssistantTurn,
  ]);

  return {
    handleQuestionOptionToggle,
    handleQuestionTextChange,
    handleQuestionSubmit,
  };
};
