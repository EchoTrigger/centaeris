import { createRoot } from "react-dom/client";
import { useRef, useState } from "react";
import { FrontendProvider } from "@centaeris/frontend-foundations/provider";
import { WorkspaceComposer } from "../../src/chat/WorkspaceComposer";
import { ConfirmDialog } from "../../src/components/ConfirmDialog";
import { i18n } from "../../src/i18n";
import "../../src/globals.css";

const parameters = new URLSearchParams(location.search);
document.documentElement.dataset.theme = parameters.get("theme") ?? "light";
await i18n.changeLanguage("en");
const choices: string[] = [], failures: string[] = [];
window.addEventListener("error", event => failures.push(event.message));
window.addEventListener("unhandledrejection", event => failures.push(String(event.reason)));
const models = [
  { id: "a", displayName: "Model A", thinkingModes: ["low", "high"] },
  { id: "b", displayName: "Model B", thinkingModes: ["low", "high"] },
];
const noop = () => {};
function Fixture() {
  const input = useRef<HTMLInputElement>(null);
  const [modelId, setModelId] = useState("a"), [effort, setEffort] = useState("high");
  const [draft, setDraft] = useState("Retained Web draft"), [confirm, setConfirm] = useState(false);
  return <FrontendProvider iconStrokeWidth={2}>
    <main className="workspaceWorkbench withoutSidebar"><section className="workspaceChatColumn"><div className="workspaceConversationPlane">
      <div className="workspaceMessages"><button type="button" onClick={() => setConfirm(true)}>Open confirmation</button></div>
      <WorkspaceComposer fileInputRef={input} isHome={false} onSubmit={event => event.preventDefault()}
        pendingAttachments={[]} pendingUploadFiles={[]} draft={draft} onDraftChange={setDraft} enterStartsNewLine={false}
        activeAgentName="Synthetic Agent" workspaceAvailable sending={false} loadingHistory={false} hasActiveAgentRun={false}
        uploadingAttachment={false} onUploadAttachment={noop} sessionId={null} currentModel={models.find(item => item.id === modelId)}
        thinkingMode={effort} onThinkingModeChange={value => { choices.push(value); setEffort(value); }}
        modelGroups={[{ provider: "synthetic", label: "Synthetic provider", models }]} modelId={modelId}
        onModelIdChange={value => { choices.push(value); setModelId(value); }} onCancelActiveAgentRun={noop} />
    </div></section></main>
    <ConfirmDialog open={confirm} title="Confirm action" message="Synthetic confirmation" onCancel={() => setConfirm(false)} onConfirm={() => setConfirm(false)} />
  </FrontendProvider>;
}
createRoot(document.getElementById("fixture")!).render(<Fixture />);
Object.assign(window, { frontendControlsFixture: { choices, failures } });
