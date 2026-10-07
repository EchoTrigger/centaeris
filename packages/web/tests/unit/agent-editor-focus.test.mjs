import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { button, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";

// Minimal DOM event/focus collaborators for the real modal hook. This verifies
// focus targets and key callbacks, not CSS, browser tab order or visual layout.
class Element {
  constructor(children = []) { this.children = children; for (const child of children) child.parentElement = this; this.listeners = new Map(); this.inert = false; this.isConnected = true; this.tabIndex = 0; }
  contains(node) { return node === this || this.children.some(child => child.contains(node)); }
  closest() { return this.inert ? this : this.parentElement?.closest() || null; }
  getClientRects() { return [{}]; }
  getAttribute() { return null; }
  querySelectorAll() { return this.children; }
  querySelector() { return null; }
  focus() { if (!this.closest()) document.activeElement = this; }
  addEventListener(name, callback) { this.listeners.set(name, callback); }
  removeEventListener(name) { this.listeners.delete(name); }
}
function dom() {
  const opener = new Element(); const background = new Element([opener]); const layer = new Element(); const body = new Element([background, layer]);
  globalThis.HTMLElement = Element; globalThis.document = { body, activeElement: opener };
  const frames = new Map(); let nextFrame = 0;
  globalThis.window = { requestAnimationFrame: callback => { frames.set(++nextFrame, callback); return nextFrame; }, cancelAnimationFrame: id => frames.delete(id) };
  return { background, layer, opener, frames() { for (const [id, callback] of frames) { frames.delete(id); callback(); } } };
}
const { AgentEditorModal } = await import(await subjectLoader({ realModal: true })(fileURLToPath(new URL("../../src/shell/AgentEditorModal.jsx", import.meta.url))));
const { useModalDialog } = await import(await subjectLoader({ realModal: true })(fileURLToPath(new URL("../../src/components/useModalDialog.js", import.meta.url))));

test("SOUL keeps background inert, receives focus, traps Tab, and Escape returns to the profile with its draft", () => {
  const browser = dom(); let closes = 0;
  const subject = renderer(AgentEditorModal, { agent: { name: "Synthetic Agent", instructions: "Original", avatarKind: "centaeris" }, heading: "Edit Agent", onClose: () => { closes++; }, onSave() {} }, environment());
  function mount() {
    const tree = subject.render(); const dialog = nodes(tree, node => node.props.role === "dialog")[0];
    assert.ok(dialog, "each editor view remains a modal dialog");
    const root = new Element([new Element(), new Element()]); root.parentElement = browser.layer;
    for (const previous of browser.layer.children) previous.isConnected = false;
    browser.layer.children = [root]; dialog.props.ref.current = root; subject.flush(); browser.frames(); return root;
  }
  mount(); button(subject.tree, "agentEditorModal.editSoulMd").props.onClick();
  const soul = mount();
  assert.equal(browser.background.inert, true); assert.ok(soul.contains(document.activeElement));
  nodes(subject.tree, node => node.type === "textarea")[0].props.onChange({ target: { value: "Unsaved SOUL draft" } });
  document.activeElement = soul.children.at(-1); let prevented = false;
  soul.listeners.get("keydown")({ key: "Tab", shiftKey: false, preventDefault: () => { prevented = true; }, stopPropagation() {} });
  assert.equal(prevented, true); assert.equal(document.activeElement, soul.children[0]);
  soul.listeners.get("keydown")({ key: "Escape", preventDefault() {}, stopPropagation() {} });
  const profile = mount(); assert.equal(closes, 0); assert.ok(profile.contains(document.activeElement));
  button(subject.tree, "workspaceContextPanel.close").props.onClick(); subject.render(); subject.flush();
  assert.ok(button(subject.tree, "agentEditorModal.discardChanges")); assert.equal(closes, 0);
  button(subject.tree, "agentEditorModal.continueEditing").props.onClick(); subject.render(); subject.flush();
  button(subject.tree, "agentEditorModal.editSoulMd").props.onClick(); mount();
  assert.equal(nodes(subject.tree, node => node.type === "textarea")[0].props.value, "Unsaved SOUL draft");
  subject.unmount(); assert.equal(browser.background.inert, false);
});

for (const preserveOpener of [true, false]) test(`modal pause/resume ${preserveOpener ? "preserves Settings entry" : "retains ordinary recapture behavior"}`, () => {
  const browser = dom(); const root = new Element([new Element(), new Element()]); root.parentElement = browser.layer; browser.layer.children = [root];
  const props = { open: true, preserveOpener, onClose() {} };
  function ModalSubject(value) { return useModalDialog(value); }
  const subject = renderer(ModalSubject, props, environment());
  const mount = () => { subject.render().current = root; subject.flush(); browser.frames(); };
  mount(); document.activeElement = root.children[1]; props.open = false; mount();
  // Inner editor restores the Edit button; resuming the outer Settings trap
  // must retain its original external opener rather than capture this button.
  const editButton = root.children[1]; document.activeElement = editButton;
  props.open = true; mount();
  root.isConnected = false; for (const child of root.children) child.isConnected = false;
  document.activeElement = null; subject.unmount(); browser.frames();
  assert.equal(document.activeElement, preserveOpener ? browser.opener : null);
});
