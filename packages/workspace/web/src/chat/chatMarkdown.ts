import { compareTranscriptBlocks, type TranscriptContentRef } from "./transcriptContract.ts";
import type { createWorkspaceTranscriptTransport } from "./transcriptTransport.ts";

// Export the complete user/assistant exchange at one server snapshot, excluding tool logs and reasoning.
export async function readChatMarkdown(
  transport: ReturnType<typeof createWorkspaceTranscriptTransport>,
  session: { id: string; title: string },
  signal: AbortSignal,
) {
  let page = await transport.loadTail(session.id, signal);
  const identity = {
    sessionId: session.id, projectionVersion: page.projectionVersion,
    projectionGeneration: page.projectionGeneration, sourceHighWater: page.sourceHighWater,
  };
  const blocks = [...page.blocks];
  while (page.olderCursor !== null) {
    page = await transport.loadOlder(identity, page.olderCursor, signal);
    blocks.push(...page.blocks);
  }
  const parts = [`# ${session.title.replace(/[\r\n]+/g, " ")}`];
  for (const block of blocks.sort(compareTranscriptBlocks)) {
    if (block.body.kind !== "userText" && block.body.kind !== "assistantText") continue;
    const content = block.body.content as { inlineContent: string | null; sourceRef: TranscriptContentRef | null };
    let text = content.inlineContent || "";
    if (content.sourceRef) {
      let offset = "0";
      while (true) {
        const range = await transport.loadContentRange({ ...identity, reference: content.sourceRef }, offset, signal);
        text += range.content;
        if (!range.hasMore) break;
        offset = range.endOffset;
      }
    }
    parts.push(`## ${block.body.kind === "userText" ? "User" : "Agent"}\n\n${text}`);
  }
  signal.throwIfAborted();
  return `${parts.join("\n\n")}\n`;
}
