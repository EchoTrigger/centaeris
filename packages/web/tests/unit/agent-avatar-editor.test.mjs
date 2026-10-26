import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { button, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";
const { AgentEditorModal } = await import(await subjectLoader()(fileURLToPath(new URL("../../src/shell/AgentEditorModal.jsx", import.meta.url))));
const original = { name: "Private Agent", description: "Original", instructions: "Keep instructions", avatarKind: "centaeris", avatarUrl: "/api/agents/private/avatar?v=old" };
function fixture() {
  const reads = [], saves = [];
  globalThis.FileReader = class {
    readAsDataURL(file) { this.file = file; reads.push(this); }
    abort() { this.aborted = true; }
    complete(value = "data:image/png;base64,aW1hZ2U=") { this.result = value; this.onload?.(); }
  };
  const subject = renderer(AgentEditorModal, { agent: original, heading: "Edit Agent", submitLabel: "Save", onClose() {}, onSave: value => saves.push(value) }, environment());
  const fileInput = () => nodes(subject.tree, node => node.type === "input" && node.props.type === "file")[0];
  const submit = () => nodes(subject.tree, node => node.type === "form")[0].props.onSubmit({ preventDefault() {} });
  return { subject, reads, saves, fileInput, submit };
}
test("custom avatar selection previews locally and submits the image with the edited profile", async () => {
  const { subject, reads, saves, fileInput, submit } = fixture(); await subject.settle();
  assert.ok(fileInput(), "the profile editor exposes an image file chooser");
  fileInput().props.onChange({ target: { files: [{ name: "portrait.png", type: "image/png", size: 7 }], value: "portrait.png" } }); await subject.settle();
  assert.equal(nodes(subject.tree, node => node.type === "button" && node.props.type === "submit")[0].props.disabled, true);
  reads[0].complete(); await subject.settle();
  const preview = nodes(subject.tree, node => node.type.name === "AgentMark" && node.props.previewSrc)[0];
  assert.equal(preview.props.previewSrc, "data:image/png;base64,aW1hZ2U=");
  submit(); assert.deepEqual(saves[0], { name: original.name, description: original.description, instructions: original.instructions, avatarKind: "centaeris", avatarDataBase64: "aW1hZ2U=" });
  subject.unmount();
});
test("rename preserves the current custom image, reset restores its preview, and default restoration explicitly clears it", async () => {
  const { subject, reads, saves, fileInput, submit } = fixture(); await subject.settle();
  nodes(subject.tree, node => node.type === "input" && node.props.name === "agentName")[0].props.onChange({ target: { value: "Renamed" } }); await subject.settle();
  submit(); assert.equal(saves[0].name, "Renamed"); assert.equal(Object.hasOwn(saves[0], "avatarDataBase64"), false);
  assert.ok(fileInput()); fileInput().props.onChange({ target: { files: [{ name: "portrait.webp", type: "image/webp", size: 7 }], value: "portrait.webp" } }); await subject.settle();
  reads[0].complete(); await subject.settle(); button(subject.tree, "agentEditorModal.reset").props.onClick(); await subject.settle();
  assert.equal(nodes(subject.tree, node => node.type.name === "AgentMark" && node.props.agent?.avatarUrl === original.avatarUrl).length, 1);
  button(subject.tree, "agentEditorModal.restoreAvatar").props.onClick(); await subject.settle(); submit();
  assert.equal(saves.at(-1).avatarDataBase64, null); assert.equal(saves.at(-1).avatarKind, "centaeris"); subject.unmount();
});
test("invalid and oversized avatar files are rejected without discarding the saved image or starting a request", async () => {
  for (const file of [{ name: "active.svg", type: "image/svg+xml", size: 7 }, { name: "large.png", type: "image/png", size: 1048577 }]) {
    const { subject, reads, saves, fileInput, submit } = fixture(); await subject.settle(); assert.ok(fileInput());
    fileInput().props.onChange({ target: { files: [file], value: file.name } }); await subject.settle();
    assert.equal(reads.length, 0); assert.equal(nodes(subject.tree, node => node.props.role === "alert").length, 1);
    submit(); assert.equal(Object.hasOwn(saves[0], "avatarDataBase64"), false); subject.unmount();
  }
});
test("reset and unmount invalidate unfinished avatar reads", async () => {
  const { subject, reads, fileInput, submit, saves } = fixture(); await subject.settle(); assert.ok(fileInput());
  fileInput().props.onChange({ target: { files: [{ name: "portrait.jpg", type: "image/jpeg", size: 7 }], value: "portrait.jpg" } }); await subject.settle();
  button(subject.tree, "agentEditorModal.reset").props.onClick(); await subject.settle(); reads[0].complete(); await subject.settle(); submit();
  assert.equal(reads[0].aborted, true); assert.equal(Object.hasOwn(saves[0], "avatarDataBase64"), false);
  fileInput().props.onChange({ target: { files: [{ name: "new.png", type: "image/png", size: 7 }], value: "new.png" } }); await subject.settle(); subject.unmount(); assert.equal(reads[1].aborted, true);
});
