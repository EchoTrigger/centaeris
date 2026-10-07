import assert from "node:assert/strict";
import { test } from "node:test";
import { agentSettingsPath, canOpenPrivateAgentSettings, agentSettingsSelection } from "../../src/agent-chat/agentSettingsNavigation.ts";

test("private Agent Settings is available to current members without administrator privilege", () => {
  for (const role of ["owner", "admin", "member"]) assert.equal(canOpenPrivateAgentSettings({ id: "workspace", role }), true);
  assert.equal(canOpenPrivateAgentSettings(null), false);
  assert.equal(canOpenPrivateAgentSettings({ id: "workspace", role: "unknown" }), false);
});
test("Agent selection is explicit; the settings list does not select a first or recent Agent", () => {
  assert.deepEqual(agentSettingsSelection("", ["agent", "other"]), { kind: "list" });
  assert.deepEqual(agentSettingsSelection("?agentId=agent", ["agent"]), { kind: "agent", agentId: "agent" });
  assert.deepEqual(agentSettingsSelection("?new=1", []), { kind: "create" });
  for (const query of ["?agentId=unknown", "?agentId=", "?agentId=agent&agentId=other", "?new=unknown", "?new=1&agentId=agent", "?new=1&new=1"]) assert.throws(() => agentSettingsSelection(query, ["agent"]));
});
test("settings navigation preserves exact workspace and Agent identities", () => {
  const url = new URL(agentSettingsPath("workspace/one", "agent:one"), "https://example.invalid");
  assert.equal(url.pathname, "/w/workspace%2Fone/settings/agents");
  assert.equal(url.searchParams.get("agentId"), "agent:one");
});
