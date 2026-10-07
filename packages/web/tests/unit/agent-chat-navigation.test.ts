import assert from "node:assert/strict";
import { test } from "node:test";
import { workspaceChatKind, agentChatPath, sessionChatPath } from "../../src/agent-chat/agentChatNavigation.ts";

test("Agent avatar entry opens persistent Agent conversation with no fresh-session query", () => {
  assert.equal(agentChatPath("workspace/one", "agent/two"), "/w/workspace%2Fone/agents/agent%2Ftwo");
  assert.equal(workspaceChatKind("agent-1", ""), "agent");
});
test("existing selected Session and explicit new-Session routes retain the ordinary Session renderer", () => {
  assert.equal(workspaceChatKind("agent-1", "?sessionId=session-1"), "session");
  assert.equal(workspaceChatKind("agent-1", "?new=1"), "session");
  assert.equal(workspaceChatKind("", ""), "session");
  assert.equal(sessionChatPath("workspace-1", "worker-agent", "work /?&"), "/w/workspace-1/agents/worker-agent?sessionId=work+%2F%3F%26");
});
test("unknown or ambiguous legacy Session selectors fail rather than turning into an Agent conversation", () => {
  for (const search of ["?sessionId=", "?sessionId=a&sessionId=b", "?new=0", "?new=1&new=1", "?new=1&sessionId=a"]) assert.throws(() => workspaceChatKind("agent-1", search));
  assert.throws(() => agentChatPath("workspace-1", ""));
  assert.throws(() => sessionChatPath("workspace-1", "", "session-1"));
});
