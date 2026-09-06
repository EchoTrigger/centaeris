import { createRoot } from "react-dom/client";
import { AgentResultStream } from "../../src/components/chat/AgentResultStream";
import type { AssistantExecutionTurn, TaskChunk } from "../../src/components/chat/types";
import "../../src/index.css";

const tool = (id: string): TaskChunk => ({ id, kind: "task", task: {
  id, title: "read", summary: "", provider: "tool", status: "done",
  normalizedInput: { path: `${id}.txt` },
  operations: [{ callId: id, toolName: "read", path: `${id}.txt`, modelContent: `Contents of ${id}` }],
} });
const turn: AssistantExecutionTurn = {
  id: "fixture-turn", agentRunId: "fixture-run", finalAnswer: "", isStreaming: false,
  chunks: [tool("a"),
    { id: "r1", kind: "reasoning", text: "先核对输入与约束，再检查工具返回的结果。", status: "done" },
    tool("b"), { id: "r2", kind: "reasoning", text: "检查已完成。", status: "done" }],
};
const container = document.getElementById("root");
if (!container) throw new Error("fixture root missing");
const longContent = new URLSearchParams(window.location.search).has("long");
const previewTurn = longContent ? { ...turn, chunks: turn.chunks.map((chunk) =>
  chunk.kind === "reasoning" && chunk.id === "r1" ? { ...chunk,
    text: Array.from({ length: 30 }, (_, index) => `段落 ${index + 1}：核对 input，保留 **重点** 和 \`code\`。`).join("\n\n"),
  } : chunk) } : turn;
createRoot(container).render(<AgentResultStream turn={previewTurn} />);
