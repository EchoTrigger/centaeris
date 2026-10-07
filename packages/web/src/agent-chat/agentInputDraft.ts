import type { AgentInputScope } from "./agentInputClient";
import { parseAgentInputAttachments, type AgentInputAttachment } from "./preparedAgentInput.ts";

type Storage = Pick<globalThis.Storage, "getItem" | "setItem" | "removeItem">;
type Draft = Readonly<{ body: string; attachments: readonly AgentInputAttachment[]; persisted: boolean }>;

// Unsent text is local tab state, separate from the durable retry identity of
// submitted input. Drafts never authorize a request or restore an inputId.
export function createAgentInputDraftStore(scope: AgentInputScope, storage?: Storage) {
  if (![scope.userId, scope.workspaceId, scope.agentId].every(value => typeof value === "string" && Boolean(value) && value.trim() === value)) throw new Error("agent_input_scope_invalid");
  const key = `centaeris.agentInputDraft.v1:${JSON.stringify([scope.userId, scope.workspaceId, scope.agentId])}`;
  const draftStorage = () => storage ?? sessionStorage;
  return {
    read(): Draft {
      try {
        const raw = draftStorage().getItem(key);
        if (raw === null) return { body: "", attachments: [], persisted: true };
        const draft = JSON.parse(raw);
        if (!draft || typeof draft !== "object" || Array.isArray(draft) || typeof draft.body !== "string") throw new Error("agent_input_draft_invalid");
        const legacy = draft.schema === "agent.input.draft.v1" && Object.keys(draft).sort().join("|") === "body|schema";
        if (!legacy && (draft.schema !== "agent.input.draft.v2" || Object.keys(draft).sort().join("|") !== "attachments|body|schema")) throw new Error("agent_input_draft_invalid");
        const attachments = legacy ? [] : parseAgentInputAttachments(draft.attachments);
        let persisted = true;
        if (legacy) {
          try { draftStorage().setItem(key, JSON.stringify({ schema: "agent.input.draft.v2", body: draft.body, attachments })); }
          catch { persisted = false; }
        }
        return { body: draft.body, attachments, persisted };
      } catch {
        return { body: "", attachments: [], persisted: false };
      }
    },
    write(body: string, attachments: readonly AgentInputAttachment[] = []): boolean {
      try {
        const valid = parseAgentInputAttachments(attachments);
        if (body === "" && !valid.length) draftStorage().removeItem(key);
        else draftStorage().setItem(key, JSON.stringify({ schema: "agent.input.draft.v2", body, attachments: valid }));
        return true;
      } catch {
        return false;
      }
    },
  };
}
