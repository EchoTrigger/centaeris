import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createMemoryRouter } from "react-router";
import { button, deferred, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";

const load = subjectLoader();
globalThis.document = { body: {} };
const { default: AgentSettings } = await import(await load(fileURLToPath(new URL("../../src/routes/AgentSettings.jsx", import.meta.url))));
const { default: AgentModelSettings } = await import(await load(fileURLToPath(new URL("../../src/routes/AgentModelSettings.jsx", import.meta.url))));
const settings = { schema: "agent.model_settings.v1", agentId: "agent", modelConfigRef: "exact-model", thinkingMode: "high", status: "configured" };
const model = { id: "exact-model", displayName: "Synthetic model", providerId: null, providerDisplayName: null, modelName: "synthetic", contextTokens: 1000, maxOutputTokens: 100, thinkingMode: "high", thinkingModes: ["low", "high"] };
const effort = tree => nodes(tree, node => (node.type.name || node.type.type?.name) === "ThinkingPicker")[0].props;
const editor = tree => nodes(tree, node => node.type.name === "AgentEditorModal")[0].props;
const editProfile = tree => {
  const props = nodes(tree, node => node.type.name === "AgentModelSettings")[0].props;
  return button(props.renderProfileActions(() => {}), "agentRoute.editAgent");
};

test("dirty model/effort choices block router departures; continue retains them and discard resumes navigation", async () => {
  let writes = 0;
  const env = environment({ request: async (path, options) => { if (options?.method === "PATCH") writes++; return path === "/api/models" ? { models: [model] } : settings; } });
  const subject = renderer(AgentModelSettings, { agentId: "agent" }, env);
  await subject.settle(); assert.equal(env.blocker.shouldBlock, false);
  effort(subject.tree).onChange("low"); await subject.settle(); assert.equal(env.blocker.shouldBlock, true);
  assert.equal(env.blocker.predicate({ currentLocation: env.location, nextLocation: env.location }), false);
  for (const nextLocation of [{ ...env.location, search: "?agentId=other" }, { ...env.location, pathname: "/w/workspace/agents/agent", search: "" }, { ...env.location, pathname: "/w/workspace/settings/general", search: "" }]) {
    assert.equal(env.blocker.predicate({ currentLocation: env.location, nextLocation }), true);
  }
  env.blocker.state = "blocked"; await subject.settle();
  button(subject.tree, "agentEditorModal.continueEditing").props.onClick(); await subject.settle();
  assert.equal(effort(subject.tree).value, "low"); assert.equal(env.blocker.resets, 1); assert.equal(writes, 0);
  env.blocker.state = "blocked"; await subject.settle();
  button(subject.tree, "agentEditorModal.discardChanges").props.onClick();
  assert.equal(env.blocker.proceeds, 1); assert.equal(writes, 0); subject.unmount();
});

test("reset and accepted model save release the departure guard; unsaved defaults remain guarded", async () => {
  const env = environment({ request: async (path, options) => path === "/api/models" ? { models: [model] } : options?.method === "PATCH" ? { ...settings, thinkingMode: JSON.parse(options.body).thinkingMode } : settings });
  const subject = renderer(AgentModelSettings, { agentId: "agent" }, env);
  await subject.settle(); button(subject.tree, "agentSettings.useDefault").props.onClick(); await subject.settle();
  assert.equal(env.blocker.shouldBlock, true);
  button(subject.tree, "agentEditorModal.reset").props.onClick(); await subject.settle(); assert.equal(env.blocker.shouldBlock, false);
  effort(subject.tree).onChange("low"); await subject.settle(); button(subject.tree, "agentRoute.saveChanges").props.onClick(); await subject.settle();
  assert.equal(env.blocker.shouldBlock, false); assert.equal(effort(subject.tree).value, "low"); subject.unmount();
});

test("the real memory router preserves the dirty Agent selection on switch, close and Back until discard", async () => {
  const env = environment({ request: async path => path === "/api/models" ? { models: [model] } : settings });
  const subject = renderer(AgentModelSettings, { agentId: "agent" }, env);
  await subject.settle(); effort(subject.tree).onChange("low"); await subject.settle();
  const selected = "/w/workspace/settings/agents?agentId=agent";
  for (const target of ["/w/workspace/settings/agents?agentId=other", "/w/workspace/agents/agent", -1]) {
    const router = createMemoryRouter([{ path: "*", element: null }], { initialEntries: ["/w/workspace/agents/agent", selected] });
    try {
      router.getBlocker("model-draft", env.blocker.predicate);
      await router.navigate(target);
      for (let tick = 0; tick < 5; tick++) await Promise.resolve();
      assert.equal(router.state.location.pathname + router.state.location.search, selected);
      assert.equal(router.state.blockers.get("model-draft").state, "blocked");
      router.state.blockers.get("model-draft").reset();
      assert.equal(effort(subject.tree).value, "low");
      await router.navigate(target);
      for (let tick = 0; tick < 5; tick++) await Promise.resolve();
      router.state.blockers.get("model-draft").proceed();
      for (let tick = 0; tick < 5; tick++) await Promise.resolve();
      assert.equal(router.state.location.pathname + router.state.location.search, target === -1 ? "/w/workspace/agents/agent" : target);
    } finally { router.dispose(); }
  }
  subject.unmount();
});

for (const action of ["reset", "save"]) test(`${action} cancels an already blocked router departure and removes the stale leave prompt`, async () => {
  const selected = "/w/workspace/settings/agents?agentId=agent";
  const router = createMemoryRouter([{ path: "*", element: null }], { initialEntries: [selected] });
  const env = environment({ router, request: async (path, options) => path === "/api/models" ? { models: [model] } : options?.method === "PATCH" ? { ...settings, thinkingMode: JSON.parse(options.body).thinkingMode } : settings });
  const subject = renderer(AgentModelSettings, { agentId: "agent" }, env);
  try {
    await subject.settle(); effort(subject.tree).onChange("low"); await subject.settle();
    await router.navigate("/w/workspace/agents/agent"); await subject.settle();
    assert.equal(router.state.blockers.get("model-draft").state, "blocked");
    assert.ok(button(subject.tree, "agentEditorModal.discardChanges"));
    button(subject.tree, action === "save" ? "agentRoute.saveChanges" : "agentEditorModal.reset").props.onClick(); await subject.settle();
    assert.equal(router.getBlocker("model-draft", env.blocker.predicate).state, "unblocked");
    assert.equal(router.state.location.pathname + router.state.location.search, selected);
    assert.equal(button(subject.tree, "agentEditorModal.discardChanges"), undefined);
    assert.equal(effort(subject.tree).value, action === "save" ? "low" : "high");
  } finally { subject.unmount(); router.dispose(); }
});

test("dirty deletion asks before DELETE; cancel preserves draft, confirm discards and clears selection before revalidation", async () => {
  const selected = "/w/workspace/settings/agents?agentId=agent";
  const router = createMemoryRouter([{ path: "*", element: null }], { initialEntries: [selected] });
  const requests = []; const order = [];
  const env = environment({ router, navigate: (path, options) => { order.push("navigate"); return router.navigate(path, options); }, request: async (path, options) => {
    requests.push([path, options?.method || "GET"]);
    if (options?.method === "DELETE") { order.push("delete"); return undefined; }
    return path === "/api/models" ? { models: [model] } : settings;
  } });
  env.revalidator.revalidate = async () => {
    order.push("revalidate"); env.agents = [];
    assert.equal(router.state.location.search, "");
    env.location = router.state.location;
  };
  const parent = renderer(AgentSettings, { onEditorOpenChange() {} }, env);
  parent.render(); parent.flush();
  const props = nodes(parent.tree, node => node.type.name === "AgentModelSettings")[0].props;
  const subject = renderer(AgentModelSettings, props, env);
  try {
    await subject.settle(); effort(subject.tree).onChange("low"); await subject.settle();
    await router.navigate("/w/workspace/agents/agent"); await subject.settle();
    assert.equal(router.state.blockers.get("model-draft").state, "blocked");
    button([parent.tree, subject.tree], "agentRoute.moveToTrash").props.onClick(); await subject.settle();
    assert.equal(requests.filter(([, method]) => method === "DELETE").length, 0);
    button(subject.tree, "agentEditorModal.continueEditing").props.onClick(); await subject.settle();
    assert.equal(requests.filter(([, method]) => method === "DELETE").length, 0);
    assert.equal(effort(subject.tree).value, "low"); assert.equal(router.state.location.search, "?agentId=agent");
    assert.equal(router.getBlocker("model-draft", env.blocker.predicate).state, "unblocked");
    button([parent.tree, subject.tree], "agentRoute.moveToTrash").props.onClick(); await subject.settle();
    button(subject.tree, "agentSettings.discardAndTrash").props.onClick(); await subject.settle();
    assert.deepEqual(order, ["delete", "navigate", "revalidate"]);
    assert.equal(requests.filter(([, method]) => method === "DELETE").length, 1);
    assert.equal(router.state.location.search, "");
    assert.equal(router.getBlocker("model-draft", env.blocker.predicate).state, "unblocked");
    assert.doesNotThrow(() => parent.render());
  } finally { subject.unmount(); parent.unmount(); router.dispose(); }
});

for (const boundary of ["response", "revalidation"]) test(`profile save completed after leaving during ${boundary} cannot navigate over Back`, async () => {
  const pending = deferred(); let signal;
  const env = environment({ request: async (_path, options) => { signal = options.signal; return boundary === "response" ? pending.promise : { agent: { id: "agent", workspaceId: "workspace" } }; } });
  if (boundary === "revalidation") env.revalidator.revalidate = () => { env.revalidationCount++; return pending.promise; };
  const subject = renderer(AgentSettings, { onEditorOpenChange() {} }, env);
  subject.render(); subject.flush(); editProfile(subject.tree).props.onClick(); await subject.settle();
  const save = editor(subject.tree).onSave({ name: "Changed" });
  await Promise.resolve(); await Promise.resolve();
  subject.unmount(); pending.resolve({ agent: { id: "agent", workspaceId: "workspace" } }); await save;
  assert.deepEqual(env.navigations, []); assert.equal(signal?.aborted, true);
  if (boundary === "response") assert.equal(env.revalidationCount, 0);
});

test("active profile save revalidates and navigates to exactly the accepted Agent", async () => {
  const env = environment({ request: async () => ({ agent: { id: "agent", workspaceId: "workspace" } }) });
  const subject = renderer(AgentSettings, { onEditorOpenChange() {} }, env);
  subject.render(); subject.flush(); editProfile(subject.tree).props.onClick(); await subject.settle();
  await editor(subject.tree).onSave({ name: "Changed" });
  assert.equal(env.revalidationCount, 1); assert.deepEqual(env.navigations, ["/w/workspace/settings/agents?agentId=agent"]); subject.unmount();
});
