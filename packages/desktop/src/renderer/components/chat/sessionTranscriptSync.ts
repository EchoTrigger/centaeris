import { getTranscriptPatches } from "../../lib/chatBridge";
import { catchUpTranscriptPatchesWith, DesktopTranscriptView, loadTranscriptPage } from "./transcriptPaging";

export async function synchronizeSessionTranscript(
  sessionId: string,
  cached?: DesktopTranscriptView | null,
  isCancelled?: () => boolean,
): Promise<DesktopTranscriptView> {
  const assertCurrent = () => { if (isCancelled?.()) throw new Error("Transcript synchronization cancelled"); };
  assertCurrent();
  if (cached) {
    if (cached.sessionId !== sessionId) throw new Error("Cached transcript session identity mismatch");
    try {
      await catchUpTranscriptPatchesWith(cached, async (request) => {
        assertCurrent();
        const response = await getTranscriptPatches(request);
        assertCurrent();
        return response;
      }, async () => { await new Promise((resolve) => setTimeout(resolve, 50)); assertCurrent(); });
      return cached;
    } catch (error) {
      assertCurrent();
      // Only the Runtime's explicit invalidation authorizes dropping paging state.
      if (!String(error).includes("transcript projectionGeneration does not match the local Runtime generation")) throw error;
    }
  }
  const page = await loadTranscriptPage({ sessionId });
  assertCurrent();
  return DesktopTranscriptView.open(page);
}
