import { expect, test } from "vitest";
import {
  buildTranscriptProcessViewModel,
  createTranscriptProjectionWork,
} from "../src/components/chat/agentTranscriptModel";
import type { AssistantExecutionTurn } from "../src/components/chat/types";

const baseline = process.env.CENTAERIS_P0_BASELINE === "1" ? test : test.skip;

const generateChunks = (count: number): AssistantExecutionTurn["chunks"] =>
  Array.from({ length: count }, (_, index) => ({
    id: `narrative:${index}`,
    kind: "narrative" as const,
    text: `line ${index}`,
    phase: "process",
    waterfall: {
      schema: "waterfall.v1",
      section: "process",
      groupId: `process:${index}`,
      displayRole: "assistant_process",
      collapsePolicy: "default",
      order: index,
    },
  }));

baseline("desktop transcript P0 scale baseline", () => {
  const samples = [100, 1_000, 10_000].map((size) => {
    const chunks = generateChunks(size);
    const work = createTranscriptProjectionWork();
    const startedAt = performance.now();
    const projected = buildTranscriptProcessViewModel({ chunks }, work);
    const elapsedMs = performance.now() - startedAt;

    expect(projected.processItems).toHaveLength(size);
    expect(work).toMatchObject({
      waterfallFilterVisits: size,
      inputChunkVisits: size,
      displayEntryVisits: size,
      sectionItemVisits: size,
    });
    const tailWork = createTranscriptProjectionWork();
    const tailStartedAt = performance.now();
    buildTranscriptProcessViewModel(
      { chunks: [...chunks, ...generateChunks(1).map((chunk) => ({ ...chunk, id: `tail:${size}` }))] },
      tailWork,
    );
    const tailElapsedMs = performance.now() - tailStartedAt;
    expect(tailWork).toMatchObject({
      waterfallFilterVisits: size + 1,
      inputChunkVisits: size + 1,
      displayEntryVisits: size + 1,
      sectionItemVisits: size + 1,
    });
    return { size, elapsedMs, work, tailElapsedMs, tailWork };
  });

  console.log(JSON.stringify({ schema: "transcript.p0.desktop.v1", samples }));
});
