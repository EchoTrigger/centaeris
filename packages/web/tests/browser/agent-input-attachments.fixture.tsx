import { useState } from "react";
import { createRoot } from "react-dom/client";
import { AgentInputComposer } from "../../src/agent-chat/AgentInputComposer";
import { AgentMessageBubble } from "../../src/agent-chat/AgentMessageBubble";
import { configureApi } from "../../src/api";
import { i18n } from "../../src/i18n";
import type { PreparedAgentInputFact } from "../../src/agent-chat/preparedAgentInput";
import "../../src/agent-chat/agent-chat.css";

configureApi({ apiBaseUrl: location.origin } as Parameters<typeof configureApi>[0]);
const query = new URLSearchParams(location.search);
await i18n.changeLanguage(query.get("locale") === "en" ? "en" : "zh-CN");
document.documentElement.dataset.theme = query.get("theme") ?? "light";
const binding = { agentId: "fixture-agent", sessionId: "fixture-coord" };
const image = { inputRef: "image", displayName: "图片.png", contentType: "image/png" };
const material = { inputRef: "material", displayName: "说明.txt", contentType: "text/plain" };
const posts: unknown[] = []; let input: PreparedAgentInputFact | null = null; let failUpload = false; let denyPreview = false;
Object.assign(window, { attachmentFixture: { posts, failUpload: () => { failUpload = true; }, denyPreview: () => { denyPreview = true; } } });
const originalFetch = window.fetch.bind(window);
window.fetch = async (request, options) => {
  const path = new URL(String(request), location.origin).pathname;
  const json = (value: unknown, status = 200) => Promise.resolve(new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } }));
  if (!path.startsWith("/api/")) return originalFetch(request, options);
  if (path === "/api/csrf") return json({ csrfToken: "isolated-fixture" });
  if (path.endsWith("/model-settings")) return json({ schema: "agent.model_settings.v1", agentId: binding.agentId, modelConfigRef: "fixture-model", thinkingMode: null, status: "configured" });
  if (path.endsWith("/coordination-session")) return json({ schema: "agent.coordination_session.v1", ...binding });
  if (path.endsWith("/uploads")) {
    if (failUpload) return json({ error: "upload_failed" }, 500);
    const files = (options?.body as FormData).getAll("files") as File[];
    return json({ libraryObjects: files.map(file => ({ id: "fixture-image", displayName: file.name })), assets: files.map(file => ({ id: "image", assetKind: "userLibraryObject", displayName: file.name, contentType: file.type, asset: { id: "fixture-image" } })) });
  }
  if (path === "/api/library") return json({ objects: [{ id: "folder", objectKind: "folder", displayName: "测试材料", contentType: "", status: "ready" }, { id: "note", objectKind: "file", displayName: material.displayName, contentType: material.contentType, status: "ready" }] });
  if (path.endsWith("/assets")) return json({ asset: { id: "material", assetKind: "userLibraryObject", displayName: material.displayName, contentType: material.contentType, asset: { id: "note" } } });
  if (path.endsWith("/inputs") && options?.method === "POST") {
    const submission = JSON.parse(String(options.body)); posts.push(submission);
    input = { inputId: submission.inputId, sequence: posts.length, createdAtMs: 1780401600000, body: submission.body, attachments: submission.attachmentRefs.map((ref: string) => ref === "image" ? image : material), read: null };
    return json({ schema: "agent.input.accepted.v1", ...binding, input });
  }
  if (path.includes("/attachments/")) {
    if (denyPreview) return json({ error: "asset_not_accessible" }, 403);
    if (path.includes("/image/")) {
      const bytes = Uint8Array.from(atob("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a4ZsAAAAASUVORK5CYII="), value => value.charCodeAt(0));
      return new Response(bytes, { headers: { "Content-Type": "image/png" } });
    }
    return new Response("Captured material contents", { headers: { "Content-Type": "text/plain" } });
  }
  throw new Error(`Unexpected isolated fixture request: ${path}`);
};
function Fixture() {
  const [accepted, setAccepted] = useState<PreparedAgentInputFact | null>(null);
  return <main style={{ maxWidth: 704, margin: "20px auto", padding: 12 }}>
    {accepted ? <AgentMessageBubble message={{ role: "user", messageId: accepted.inputId, text: accepted.body, authorLabel: "Fixture user", isLatest: true, attachments: accepted.attachments, attachmentBinding: { agentId: binding.agentId, inputId: accepted.inputId } }} readLabel="Read" onPreviewSession={() => {}} /> : null}
    <AgentInputComposer agentId={binding.agentId} workspaceId="fixture-workspace" userId="fixture-user" sessionId={binding.sessionId} onAccepted={() => setAccepted(input)} onAuthorityLost={() => {}} />
  </main>;
}
createRoot(document.getElementById("root")!).render(<Fixture />);
