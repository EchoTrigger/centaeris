import { createRoot } from "react-dom/client";
import { useRef, useState } from "react";
import { FrontendProvider } from "@centaeris/ui/provider";
import { WorkspaceComposer } from "../../src/chat/WorkspaceComposer";
import { ChatTitle } from "../../src/shell/ChatTitle";
import "../../src/shell/conversation-dock.css";
import { ConfirmDialog } from "../../src/components/ConfirmDialog";
import { i18n } from "../../src/i18n";
import { configureApi } from "../../src/api";
import "../../src/globals.css";

const parameters = new URLSearchParams(location.search);
document.documentElement.dataset.theme = parameters.get("theme") ?? "light";
await i18n.changeLanguage("en");
const floating = parameters.has("floating");
const longTitle = "Review the complete release materials and permissions across all departments before publication";
const choices: string[] = [], failures: string[] = [];
configureApi({ apiBaseUrl: location.origin });
window.fetch = async input => {
  if (String(input) === `${location.origin}/api/workspaces/synthetic/sessions/test/issue-origin`) return Response.json({ origin: null });
  throw new Error(`Unexpected frontend controls request: ${String(input)}`);
};
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
  const [empty, setEmpty] = useState(false);
  return <FrontendProvider iconStrokeWidth={2}>
    <main className={`workspaceWorkbench withoutSidebar${floating ? " isBrowsing isChatFloating" : ""}`}>
      <header className="workspaceTopbar"><div className="workspaceTopbarStart"><ChatTitle session={{ id: "test", workspaceId: "synthetic", title: longTitle }} actions={{ onTogglePin: noop }} titleControl={<button className="conversationSwitcher"><span title={longTitle}>{longTitle}</span></button>} /></div><div className="conversationWindowActions"><button className="workspacePanelToggle" aria-label="Expand chat">↗</button></div></header>
      <section className="workspaceChatColumn"><div className={`workspaceConversationPlane${empty ? " isEmpty" : ""}`}>
      <div className="workspaceMessages"><button type="button" data-action="confirm" onClick={() => setConfirm(true)}>Open confirmation</button><button type="button" data-action="toggle-empty" onClick={() => setEmpty(value => !value)}>Toggle empty conversation</button></div>
      <WorkspaceComposer fileInputRef={input} isHome={false} onSubmit={event => event.preventDefault()}
        pendingAttachments={[]} pendingUploadFiles={[]} draft={draft} onDraftChange={setDraft} enterStartsNewLine={false}
        workspaceName="Synthetic workspace" workspaceAvailable sending={false} loadingHistory={false} hasActiveAgentRun={false}
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
