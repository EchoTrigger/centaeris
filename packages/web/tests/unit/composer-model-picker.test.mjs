import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { elements, createJsxSubjectLoader } from "./jsxSubject.mjs";

// Composition stays controlled by Web; mounted menu interactions are covered
// separately in tests/components/frontend-controls.test.tsx.
const pickers = `import { createElement } from ${JSON.stringify(import.meta.resolve("react"))};
  export const thinkingModeLabels=t=>({low:t('appRoute.low'),high:t('appRoute.high')});
  export const ModelPicker=props=>createElement('model-picker',props);
  export const ThinkingPicker=props=>createElement('thinking-picker',props);`;
const load = createJsxSubjectLoader(new Map([["../components/ModelSelectionPickers",
  `data:text/javascript;base64,${Buffer.from(pickers).toString("base64")}`]]));
const { WorkspaceComposer } = await import(await load(fileURLToPath(new URL("../../src/chat/WorkspaceComposer.jsx", import.meta.url))));
const model = { id: "model-exact", displayName: "Chosen model", thinkingModes: ["low", "high"] };
function subject(overrides = {}) {
  return WorkspaceComposer.type({ pendingAttachments: [], pendingUploadFiles: [], draft: "Retained draft", modelGroups: [{ provider: "provider", label: "Provider", models: [model] }], modelId: model.id, currentModel: model, thinkingMode: "high", fileInputRef: { current: null }, ...overrides });
}
const control = (tree, kind) => elements(tree, node => node.type === `${kind}-picker`)[0];

test("ordinary Session model and effort pickers are controlled and never choose during render", () => {
  const choices = [];
  const tree = subject({ onModelIdChange: id => choices.push(id), onThinkingModeChange: mode => choices.push(mode) });
  assert.deepEqual(choices, []);
  assert.equal(control(tree, "model").props.value, model.id);
  assert.equal(control(tree, "model").props.model, model);
  assert.equal(control(tree, "thinking").props.value, "high");
  assert.equal(elements(tree, node => node.type === "textarea")[0].props.value, "Retained draft");
});

test("ordinary Session model and effort choices retain separate callbacks", () => {
  const models = [], efforts = [];
  const tree = subject({ onModelIdChange: id => models.push(id), onThinkingModeChange: mode => efforts.push(mode) });
  control(tree, "model").props.onChange("model-next");
  assert.deepEqual(models, ["model-next"]);
  assert.deepEqual(efforts, []);
  control(tree, "thinking").props.onChange("low");
  assert.deepEqual(efforts, ["low"]);
});

test("ordinary Session sending and active-Run states retain existing picker restrictions", () => {
  assert.equal(control(subject({ hasActiveAgentRun: true }), "thinking").props.disabled, true);
  assert.equal(Boolean(control(subject({ hasActiveAgentRun: true }), "model").props.disabled), false);
  for (const kind of ["model", "thinking"]) assert.equal(control(subject({ sending: true }), kind).props.disabled, true);
  assert.equal(control(subject({ modelGroups: [], currentModel: undefined }), "model").props.disabled, true);
  assert.equal(control(subject({ currentModel: { ...model, thinkingModes: [] } }), "thinking").props.disabled, true);
});


test("a known Run keeps an enabled Stop control during submission and history loading", () => {
  let stops=0;
  const tree=subject({hasActiveAgentRun:true,activeAgentRunId:"queued-run",sending:true,loadingHistory:true,onCancelActiveAgentRun:()=>stops++});
  const stop=elements(tree,node=>node.type==="button" && node.props["aria-label"]==="appRoute.stop")[0];
  assert.ok(stop);
  assert.equal(Boolean(stop.props.disabled),false);
  stop.props.onClick();assert.equal(stops,1);
});

test("a pending cancellation remains actionable and describes its progress", () => {
  let stops = 0;
  const tree=subject({hasActiveAgentRun:true,activeAgentRunId:"queued-run",cancellingAgentRunId:"queued-run", controlStatus:"Stopping this run", onCancelActiveAgentRun:()=>stops++});
  const stop=elements(tree,node=>node.type==="button" && node.props["aria-label"]==="appRoute.stop")[0];
  assert.equal(Boolean(stop.props.disabled),false);
  assert.equal(stop.props["aria-busy"],true);
  stop.props.onClick();
  assert.equal(stops,1);
  assert.ok(elements(tree,node=>node.props?.role==="status" && node.props.children==="Stopping this run").length);
});

test("send and draft remain actionable while validation or submission is pending", () => {
  for (const state of [{draft:""}, {modelId:""}, {sending:true}, {loadingHistory:true}, {workspaceAvailable:false}]) {
    const tree=subject(state);
    const send=elements(tree,node=>node.type==="button" && node.props.type==="submit")[0];
    assert.equal(Boolean(send.props.disabled),false);
    assert.equal(Boolean(elements(tree,node=>node.type==="textarea")[0].props.disabled),false);
  }
});

test("a submission awaiting its Run identity still offers Stop", () => {
  let stops=0;
  const tree=subject({hasActiveAgentRun:true,activeAgentRunId:"",controlBusy:true,onCancelActiveAgentRun:()=>stops++});
  const stop=elements(tree,node=>node.type==="button" && node.props["aria-label"]==="appRoute.stop")[0];
  assert.equal(Boolean(stop.props.disabled),false);
  assert.equal(stop.props["aria-busy"],true);
  stop.props.onClick();assert.equal(stops,1);
});
