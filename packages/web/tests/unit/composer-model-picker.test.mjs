import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { elements, loadJsxSubject } from "./jsxSubject.mjs";

const { WorkspaceComposer } = await import(await loadJsxSubject(fileURLToPath(new URL("../../src/chat/WorkspaceComposer.jsx", import.meta.url))));
const model = { id: "model-exact", displayName: "Chosen model", thinkingModes: ["low", "high"] };
function subject(overrides = {}) {
  return WorkspaceComposer.type({ pendingAttachments: [], pendingUploadFiles: [], draft: "", modelGroups: [{ provider: "provider", label: "Provider", models: [model] }], modelId: model.id, currentModel: model, thinkingMode: "high", fileInputRef: { current: null }, ...overrides });
}
const control = (tree, label) => elements(tree, node => node.type === "summary" && node.props["aria-label"] === label)[0];
const options = tree => elements(tree, node => node.type === "button" && typeof node.props["aria-pressed"] === "boolean");

test("ordinary Session model and effort pickers are controlled and never choose during render", () => {
  const choices = [];
  const tree = subject({ onModelIdChange: id => choices.push(id), onThinkingModeChange: mode => choices.push(mode) });
  assert.deepEqual(choices, []);
  const modelControl = control(tree, "appRoute.aiModel");
  assert.equal(elements(modelControl, node => node.type === "span")[0].props.children, "Chosen model");
  const pressed = options(tree).filter(option => option.props["aria-pressed"]);
  assert.deepEqual(pressed.map(option => elements(option, node => node.type === "span")[0].props.children), ["appRoute.high", "Chosen model"]);
});

test("ordinary Session explicit choices invoke callbacks and close only the picker", () => {
  const choices = [];
  const tree = subject({ onModelIdChange: id => choices.push(id), onThinkingModeChange: mode => choices.push(mode) });
  const closed = [];
  const event = { currentTarget: { closest: tag => ({ removeAttribute: name => closed.push([tag, name]) }) } };
  const buttons = options(tree);
  buttons[0].props.onClick(event); buttons.at(-1).props.onClick(event);
  assert.deepEqual(choices, ["low", "model-exact"]);
  assert.deepEqual(closed, [["details", "open"], ["details", "open"]]);
});

test("ordinary Session sending and active-Run states retain existing picker restrictions", () => {
  assert.equal(control(subject({ hasActiveAgentRun: true }), "appRoute.reasoningEffort").props["aria-disabled"], true);
  assert.equal(control(subject({ hasActiveAgentRun: true }), "appRoute.aiModel").props["aria-disabled"], undefined);
  for (const label of ["appRoute.aiModel", "appRoute.reasoningEffort"]) {
    let prevented = false;
    control(subject({ sending: true }), label).props.onClick({ preventDefault: () => { prevented = true; } });
    assert.equal(prevented, true);
  }
  assert.equal(control(subject({ modelGroups: [], currentModel: undefined }), "appRoute.aiModel").props["aria-disabled"], true);
});

test("ordinary Session picker Escape restores summary focus and blur inside retains the menu", () => {
  const details = elements(subject(), node => node.type === "details" && Boolean(node.props.onKeyDown))[0];
  const actions = [];
  const currentTarget = { removeAttribute: name => actions.push(name), querySelector: name => ({ focus: () => actions.push(name) }), contains: value => value === "inside" };
  details.props.onBlur({ currentTarget, relatedTarget: "inside" });
  assert.deepEqual(actions, []);
  details.props.onKeyDown({ currentTarget, key: "Escape", preventDefault: () => actions.push("prevent") });
  assert.deepEqual(actions, ["prevent", "open", "summary"]);
});
