import { createRoot } from "react-dom/client";
import "../../src/index.css";
import { AgentProcessTranscript } from "../../src/components/chat/AgentTranscriptSections";
import { buildTranscriptProcessViewModel } from "../../src/components/chat/agentTranscriptModel";

document.documentElement.dataset.theme = new URLSearchParams(location.search).get("theme") || "light";
const reasons = ["provider_timeout", "exceeded retry limit, last status: 429 Too Many Requests", "<b>failure</b> 中文 " + "provider_fault_".repeat(20)];
createRoot(document.getElementById("fixture")!).render(<main style={{ padding: 16 }}>
  <AgentProcessTranscript agentRunId="fixture" onOpenWorkspacePath={undefined} processTranscript={buildTranscriptProcessViewModel({
    chunks: reasons.map((text, index) => ({ id: `failure-${index}`, kind: "narrative", text, tone: "error", runFailure: "failed" })),
  })} />
</main>);
