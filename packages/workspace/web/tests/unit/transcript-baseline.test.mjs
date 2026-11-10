import assert from "node:assert/strict";
import test from "node:test";
import {
  applyStreamEntry,
  createTranscriptProjectionWork,
  validateHistoryPage,
} from "../../src/chat/sessionEvents.ts";
import {
  buildAgentRunSections,
  createAgentRunPresentationWork,
} from "../../src/chat/agentRunPresentation.mjs";

const sessionId = "session:baseline";
const agentRunId = "agent-run:baseline";

function event(type, sequence, payload) {
  return {
    sequence,
    event: {
      schemaVersion: "session.event.v1",
      eventVersion: 1,
      sequence,
      type,
      eventId: `event:baseline:${sequence}`,
      sessionId,
      turnId: "turn:baseline",
      agentRunId,
      createdAtMs: sequence,
      payload,
    },
  };
}

function generateEvents(size) {
  return Array.from({ length: size }, (_, index) => index === 0
    ? event("agent_run_started", 1, { userObjective: "measure projection" })
    : event("phase_event", index + 1, {
      stage: "model_process_summary",
      message: `phase ${index}`,
    }));
}

function historyPage(events) {
  return {
    schema: "session.history.page.v1",
    session: { id: sessionId, workspaceId: "workspace:baseline" },
    agentRuns: [{
      id: agentRunId,
      status: "running",
      model: { id: "model:baseline", displayName: "baseline" },
      createdAt: "2026-09-13T00:00:00Z",
      startedAt: "2026-09-13T00:00:00Z",
      completedAt: null,
      events,
      live: null,
      streamCursor: "1-0",
      citations: [],
      citationSequence: 0,
    }],
    nextCursor: null,
    hasMore: false,
  };
}

function liveEntry(revision, text, cursor = `${revision}-0`) {
  return {
    cursor,
    item: {
      schema: "session.stream.item.v1",
      kind: "live",
      agentRunId,
      afterSequence: 0,
      revision,
      turnId: "turn:baseline",
      messageId: "message:baseline",
      text,
    },
  };
}

test("Workspace Web transcript projection baseline", () => {
  const samples = [100, 1_000, 10_000].map((size) => {
    const historyWork = createTranscriptProjectionWork();
    const historyStartedAt = performance.now();
    const view = validateHistoryPage(historyPage(generateEvents(size)), {}, historyWork).agentRuns[0];
    const historyElapsedMs = performance.now() - historyStartedAt;

    const liveWork = createTranscriptProjectionWork();
    const liveStartedAt = performance.now();
    const updated = applyStreamEntry(view, liveEntry(1, "tail"), liveWork);
    const liveElapsedMs = performance.now() - liveStartedAt;

    const presentationWork = createAgentRunPresentationWork();
    const presentationStartedAt = performance.now();
    const sections = buildAgentRunSections(
      updated.messages,
      updated.activities,
      updated.reasoningBlocks,
      (value) => value,
      presentationWork,
    );
    const presentationElapsedMs = performance.now() - presentationStartedAt;

    assert.equal(historyWork.committedEventVisits, size);
    assert.equal(liveWork.committedEventVisits, size);
    return {
      size,
      historyElapsedMs,
      liveElapsedMs,
      presentationElapsedMs,
      historyWork,
      liveWork,
      presentationWork,
      sectionCount: sections.length,
    };
  });

  console.log(JSON.stringify({
    schema: "transcript.baseline.workspace_web.v1",
    samples,
  }));
});
