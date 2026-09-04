import { useEffect, useState } from "react";
import {
  openAgentStream,
  type AgentStreamPayload,
  type SessionEvent,
} from "../../lib/chatBridge";
import { AgentResultStream } from "./AgentResultStream";
import { appendNarrativeChunk } from "./chatAreaModel";
import {
  applySessionEventToAssistantTurn,
  buildSessionHydrationSnapshot,
  getSessionEventId,
  getTerminalSessionEventStatus,
  isRecord,
} from "./chatRuntimeModel";
import type { ChatMessage } from "./types";

type AgentSessionPreviewProps = {
  sessionId: string;
  onOpenWorkspacePath?: (
    path: string,
    options?: { startLine?: number; endLine?: number; taskId?: string },
  ) => void;
};

const updateAssistantTurn = (
  messages: ChatMessage[],
  messageId: string,
  update: (
    turn: Extract<ChatMessage, { role: "assistant" }>["turn"],
  ) => Extract<ChatMessage, { role: "assistant" }>["turn"],
): ChatMessage[] => messages.map((message) =>
  message.id === messageId && message.role === "assistant"
    ? { ...message, turn: update(message.turn) }
    : message,
);

function AgentSessionPreviewLifecycle({
  sessionId,
  onOpenWorkspacePath,
  onReload,
}: AgentSessionPreviewProps & { onReload: () => void }) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [status, setStatus] = useState<
    "loading" | "queued" | "running" | "done" | "error" | "cancelled"
  >("loading");
  const [error, setError] = useState("");

  useEffect(() => {
    let disposed = false;
    let closeStream: (() => void) | null = null;
    setError("");
    setStatus("loading");

    void buildSessionHydrationSnapshot(sessionId)
      .then((snapshot) => {
        if (disposed) return;
        setMessages(snapshot.messages);
        setStatus(snapshot.activeReplay?.status === "queued" ? "queued" : snapshot.activeReplay ? "running" : "done");
        if (!snapshot.activeReplay) return;

        const messageId = snapshot.activeReplay.messageId;
        const seenEventIds = new Set(
          snapshot.activeReplay.seedPayloads
            .filter((payload) => payload.type === "session_event" && isRecord(payload.event))
            .map((payload) => getSessionEventId(payload.event as SessionEvent))
            .filter(Boolean),
        );
        const handlePayload = (payload: AgentStreamPayload) => {
          setStatus((current) => current === "queued" ? "running" : current);
          if (payload.type === "session_event" && isRecord(payload.event)) {
            const event = payload.event as SessionEvent;
            const eventId = getSessionEventId(event);
            if (eventId && seenEventIds.has(eventId)) return;
            if (eventId) seenEventIds.add(eventId);
            let terminalStatus: ReturnType<
              typeof getTerminalSessionEventStatus
            > = null;
            try {
              terminalStatus = getTerminalSessionEventStatus(event);
            } catch (cause) {
              setError(cause instanceof Error ? cause.message : String(cause));
              setStatus("error");
              return;
            }
            setMessages((current) => updateAssistantTurn(
              current,
              messageId,
              (turn) => applySessionEventToAssistantTurn(turn, event),
            ));
            if (terminalStatus) {
              setStatus(
                terminalStatus === "succeeded"
                  ? "done"
                  : terminalStatus === "failed"
                    ? "error"
                    : "cancelled",
              );
            }
            return;
          }
          if (payload.type === "error") {
            const message = String(payload.message || "Agent stream failed");
            setMessages((current) => updateAssistantTurn(
              current,
              messageId,
              (turn) => ({
                ...appendNarrativeChunk(turn, message, "error"),
                isStreaming: false,
                activity: undefined,
              }),
            ));
            setStatus("error");
          }
        };
        const stream = openAgentStream(
          snapshot.activeReplay.agentRunId,
          handlePayload,
          (streamError) => {
            if (disposed) return;
            setError(streamError.message);
            setStatus("error");
          },
        );
        closeStream = stream.close;
      })
      .catch((cause: unknown) => {
        if (disposed) return;
        setError(cause instanceof Error ? cause.message : String(cause));
        setStatus("error");
      });

    return () => {
      disposed = true;
      closeStream?.();
    };
  }, [sessionId]);

  return (
    <div className="agentSessionPreview">
      <div className="agentSessionPreviewMessages">
        {status === "loading" ? <div className="summaryPanelHint">正在读取 Agent 会话...</div> : null}
        {error ? (
          <div className="summaryPanelHint is-error">
            {error}
            <button type="button" onClick={onReload}>重新加载</button>
          </div>
        ) : null}
        {messages.map((message) => message.role === "user" ? (
          <div className="agentSessionPreviewUser" key={message.id}>{message.text}</div>
        ) : (
          <div className="agentSessionPreviewAssistant" key={message.id}>
            <AgentResultStream
              turn={message.turn}
              onOpenWorkspacePath={onOpenWorkspacePath}
            />
          </div>
        ))}
      </div>
      <div className={`agentSessionPreviewStatus is-${status}`} aria-live="polite">
        <span aria-hidden="true" />
        <strong>Agent {status === "loading" ? "读取中" : status === "queued" ? "排队中" : status === "running" ? "运行中" : status === "done" ? "已完成" : status === "cancelled" ? "已取消" : "失败"}</strong>
      </div>
    </div>
  );
}

export function AgentSessionPreview(props: AgentSessionPreviewProps) {
  const [reloadVersion, setReloadVersion] = useState(0);
  return (
    <AgentSessionPreviewLifecycle
      {...props}
      key={`${props.sessionId}:${reloadVersion}`}
      onReload={() => setReloadVersion((value) => value + 1)}
    />
  );
}
