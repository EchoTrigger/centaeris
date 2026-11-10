import { createRoot } from "react-dom/client";
import { AgentRunRow } from "../../src/chat/AgentRunRow";
import { LanguageSelector } from "../../src/components/LanguageSelector";
import { createChatViewStore } from "../../src/chat/chatViewStore";
import { hydrateAgentRun } from "../../src/chat/sessionEvents";
import "../../src/globals.css";

const store = createChatViewStore();
const tool = (id, sequence) => ({
  activityId: id, callId: id, sequence, toolName: "read", status: "completed",
  call: { normalizedInput: { path: `${id}.txt` }, displayTarget: `${id}.txt` },
  result: { modelContent: `Contents of ${id}`, operations: [] },
});
// Synthetic view updates exercise future live behavior; committedHistory uses the real reducer.
const run = {
  ...hydrateAgentRun({
    id: "fixture-run", status: "running", model: {},
    createdAt: "2026-09-05T00:00:00Z", startedAt: "2026-09-05T00:00:00Z",
    completedAt: null, events: [], live: null, streamCursor: "0-0", citations: [], citationSequence: 0,
  }),
  activities: [tool("a", 1), tool("b", 3)],
  messages: [{ messageId: "user", role: "user", text: "Inspect this task", phase: "user", sequence: 0, attachments: [], artifacts: [] }],
  reasoningBlocks: [
    { id: "r1", sequence: 2, text: "Inspect inputs", status: "streaming" },
    { id: "r2", sequence: 4, text: "Check the result", status: "done" },
  ],
};
store.replaceAll([run]);
let root = createRoot(document.getElementById("root"));
const render = () => root.render(<><LanguageSelector /><AgentRunRow store={store} agentRunId={run.id} /></>);
render();
window.reasoningFixture = {
  liveSnapshot(revision, text) {
    store.replaceAgentRun(hydrateAgentRun({
      id: run.id, status: "running", model: {}, createdAt: "2026-09-05T00:00:00Z", startedAt: "2026-09-05T00:00:00Z", completedAt: null,
      events: [], streamCursor: "0-0", citations: [], citationSequence: 0,
      live: { messageId: "answer", turnId: "fixture-turn", afterSequence: 0, revision, text: "Streaming answer", reasoning: { blockId: "reasoning:request-1", requestId: "request-1", text } },
    }));
  },
  committedHistory(status = "done") {
    const event = (type, sequence, payload) => ({ sequence, event: {
      schemaVersion: "session.event.v1", eventVersion: 1, type, sequence,
      eventId: `event:${sequence}`, sessionId: "fixture-session", agentRunId: run.id,
      turnId: "fixture-turn", createdAtMs: sequence, payload,
    } });
    store.replaceAgentRun(hydrateAgentRun({
      id: run.id, status: status === "interrupted" ? "cancelled" : "completed", model: {}, createdAt: run.createdAt,
      startedAt: run.startedAt, completedAt: "2026-09-05T00:00:02Z",
      live: null, streamCursor: "1-0", citations: [], citationSequence: 0, events: [
        event("reasoning_block", 1, { blockId: "reasoning:request-1", requestId: "request-1", text: "Committed thinking", status }),
        event("assistant_message", 2, { messageId: "answer", modelMarkdown: "Committed answer", artifactRefs: [], status: "done" }),
      ],
    }));
  },
  longContent() {
    store.replaceAgentRun({ ...store.getAgentRunSnapshot(run.id),
      reasoningBlocks: [{ ...run.reasoningBlocks[0], text: Array.from({ length: 30 }, (_, index) => `段落 ${index + 1}：核对 input，保留 **重点** 和 \`code\`。`).join("\n\n") }],
    });
  },
  reconnect() { store.updateConnection(run.id, "reconnecting"); },
  resume() { store.updateConnection(run.id, "running"); },
  complete() {
    store.replaceAgentRun({ ...store.getAgentRunSnapshot(run.id), status: "completed",
      reasoningBlocks: run.reasoningBlocks.map((block) => block.id === "r1"
        ? { ...block, status: "done", text: "Inspect inputs and constraints" } : block),
    });
  },
  remount() {
    root.unmount();
    root = createRoot(document.getElementById("root"));
    render();
  },
};
