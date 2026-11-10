import { createRoot } from "react-dom/client";
import { TranscriptBlockRow } from "../../src/chat/TranscriptBlockContent";
import { createTranscriptViewStore } from "../../src/chat/transcriptViewStore";

document.documentElement.dataset.theme = new URLSearchParams(location.search).get("theme") || "light";
const store = createTranscriptViewStore();
store.openTail({
  schema: "transcript.page.v1", sessionId: "fixture", projectionVersion: "transcript.projection.v1", projectionGeneration: "fixture",
  sourceHighWater: "2", olderCursor: null, hasOlder: false, resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: "2" }],
  blocks: ["provider_timeout", "exceeded retry limit, last status: 429 Too Many Requests"].map((text, index) => ({
    blockId: `error-${index}`, blockRevision: "1", orderKey: { sourceSequence: String(index + 1), ordinal: 0 },
    body: { kind: "notice", noticeType: "run_boundary", status: "completed", content: { inlineContent: text, sourceRef: null } },
    presentation: { sourceType: "agent_run_failed", agentRunId: "fixture", observedAtMs: 1000, displayTarget: null, durationMs: null, operation: null },
  })),
});
createRoot(document.getElementById("fixture")!).render(
  <div className="workspaceTranscriptBlocks"><TranscriptBlockRow store={store} blockId="error-0" /><TranscriptBlockRow store={store} blockId="error-1" /></div>,
);
