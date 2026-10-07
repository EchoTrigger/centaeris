export type RunPreviewFact = Readonly<{ agentRunId: string; status: "queued" | "running" | "completed" | "failed" | "cancelled"; sourceType: "hostedRun" | "sessionEvent" | "preAdmissionCancellation"; eventId: string | null; createdAtMs: number }>;
export type FilePreviewFact = Readonly<{ inputRef: string | null; agentRunId: string; ownerKind: "artifact" | "sourceObject" | "userLibraryObject"; objectRef: string; sourceVersion: string; sha256: string; displayName: string; contentType: string; sizeBytes: number; previewUrl: string; downloadUrl: string }>;
export type MessageFileBinding = Readonly<{ agentId: string; sessionId: string; agentRunId: string; inputRefs: readonly string[] }>;
export type SessionPreviewPage = Readonly<{ schema: "session.preview.v1"; sessionId: string; runFact: RunPreviewFact | null; outputs: readonly FilePreviewFact[]; nextAfterArtifactId: string | null; hasMore: boolean }>;
function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("agent_preview_invalid");
  return value as Record<string, unknown>;
}
function exact(value: Record<string, unknown>, fields: string) {
  if (Object.keys(value).sort().join("|") !== fields.split("|").sort().join("|")) throw new Error("agent_preview_invalid");
}
const opaque = (value: unknown): value is string => typeof value === "string" && Boolean(value) && value.trim() === value;
function runFact(value: unknown): RunPreviewFact | null {
  if (value === null) return null;
  const row = record(value); exact(row, "agentRunId|status|sourceType|eventId|createdAtMs");
  const terminal = ["completed", "failed", "cancelled"].includes(String(row.status));
  if (!opaque(row.agentRunId) || !["queued", "running", "completed", "failed", "cancelled"].includes(String(row.status))
    || typeof row.createdAtMs !== "number" || !Number.isSafeInteger(row.createdAtMs) || row.createdAtMs < 0 || !Number.isFinite(new Date(row.createdAtMs).getTime())
    || (row.sourceType === "hostedRun" ? terminal || row.eventId !== null
      : row.sourceType === "sessionEvent" ? !terminal || !opaque(row.eventId)
        : row.sourceType !== "preAdmissionCancellation" || row.status !== "cancelled" || row.eventId !== null)) throw new Error("agent_preview_run_fact_invalid");
  return structuredClone(row) as RunPreviewFact;
}
function fileFact(value: unknown, prefix: (file: FilePreviewFact) => string): FilePreviewFact {
  const row = record(value); exact(row, "inputRef|agentRunId|ownerKind|objectRef|sourceVersion|sha256|displayName|contentType|sizeBytes|previewUrl|downloadUrl");
  if (![row.agentRunId, row.objectRef, row.displayName, row.contentType].every(opaque) || (row.inputRef !== null && !opaque(row.inputRef))
    || !["artifact", "sourceObject", "userLibraryObject"].includes(String(row.ownerKind))
    || typeof row.sourceVersion !== "string" || row.sourceVersion.length > 20 || !/^[1-9][0-9]*$/u.test(row.sourceVersion)
    || typeof row.sha256 !== "string" || !/^sha256:[0-9a-f]{64}$/u.test(row.sha256)
    || typeof row.sizeBytes !== "number" || !Number.isSafeInteger(row.sizeBytes) || row.sizeBytes < 0) throw new Error("agent_preview_file_invalid");
  const file = structuredClone(row) as FilePreviewFact;
  for (const action of ["preview", "download"] as const) {
    const href = file[action === "preview" ? "previewUrl" : "downloadUrl"];
    if (typeof href !== "string" || !href.startsWith("/api/")) throw new Error("agent_preview_url_invalid");
    const url = new URL(href, "https://preview.invalid");
    if (url.origin !== "https://preview.invalid" || url.hash || url.pathname !== prefix(file)+`/${action}`
      || [...url.searchParams.keys()].sort().join("|") !== "lang|sha256|sourceVersion" || url.searchParams.get("lang") !== "zh-CN"
      || url.searchParams.get("sourceVersion") !== file.sourceVersion || url.searchParams.get("sha256") !== file.sha256) throw new Error("agent_preview_url_invalid");
  }
  return file;
}
export function sessionPreviewPath(sessionId: string, afterArtifactId: string | null = null, limit = 50) {
  if (!opaque(sessionId) || afterArtifactId !== null && !opaque(afterArtifactId) || !Number.isSafeInteger(limit) || limit < 1 || limit > 100) throw new Error("agent_preview_request_invalid");
  const query = new URLSearchParams({ limit: String(limit) });
  if (afterArtifactId !== null) query.set("afterArtifactId", afterArtifactId);
  return `/api/sessions/${encodeURIComponent(sessionId)}/preview?${query}`;
}
export function parseSessionPreview(value: unknown, sessionId: string, afterArtifactId: string | null = null, limit = 50): SessionPreviewPage {
  sessionPreviewPath(sessionId, afterArtifactId, limit);
  const page = record(value); exact(page, "schema|sessionId|runFact|outputs|nextAfterArtifactId|hasMore");
  if (page.schema !== "session.preview.v1" || page.sessionId !== sessionId || !Array.isArray(page.outputs) || page.outputs.length > limit || typeof page.hasMore !== "boolean") throw new Error("agent_preview_invalid");
  const ids = new Set<string>();
  const outputs = page.outputs.map(value => {
    const file = fileFact(value, row => `/api/sessions/${encodeURIComponent(sessionId)}/outputs/${encodeURIComponent(row.objectRef)}`);
    if (file.ownerKind !== "artifact" || file.inputRef !== null || file.objectRef === afterArtifactId || ids.has(file.objectRef)) throw new Error("agent_preview_output_identity_invalid");
    ids.add(file.objectRef); return file;
  });
  if (page.hasMore ? !outputs.length || page.nextAfterArtifactId !== outputs.at(-1)?.objectRef || outputs.length !== limit : page.nextAfterArtifactId !== null) throw new Error("agent_preview_cursor_invalid");
  return { schema: "session.preview.v1", sessionId, runFact: runFact(page.runFact), outputs, hasMore: page.hasMore, nextAfterArtifactId: page.nextAfterArtifactId as string | null };
}
export function parseMessageFiles(value: unknown, messageId: string, binding: MessageFileBinding): readonly FilePreviewFact[] {
  if (![messageId, binding.agentId, binding.sessionId, binding.agentRunId].every(opaque) || binding.inputRefs.length > 8 || !binding.inputRefs.every(opaque)) throw new Error("agent_preview_binding_invalid");
  const page = record(value); exact(page, "schema|agentId|sessionId|messageId|agentRunId|files");
  if (page.schema !== "agent.message.files.v1" || page.agentId !== binding.agentId || page.sessionId !== binding.sessionId
    || page.messageId !== messageId || page.agentRunId !== binding.agentRunId || !Array.isArray(page.files) || page.files.length > 8) throw new Error("agent_preview_binding_invalid");
  const files = page.files.map(value => fileFact(value, row => `/api/agents/${encodeURIComponent(binding.agentId)}/messages/${encodeURIComponent(messageId)}/files/${encodeURIComponent(row.inputRef ?? "")}`));
  const refs = [...new Set(binding.inputRefs)];
  if (files.length !== refs.length || files.some((file, index) => file.inputRef !== refs[index] || file.agentRunId !== binding.agentRunId)) throw new Error("agent_preview_binding_invalid");
  return files;
}
