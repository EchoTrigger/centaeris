import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { button, deferred, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";

const { SessionInstructions } = await import(await subjectLoader()(fileURLToPath(new URL("../../src/shell/SessionInstructions.jsx", import.meta.url))));
const session = { id: "session", workspaceId: "workspace", instructions: "" };

test("Session instructions save exactly the current Session's independent text", async () => {
  const calls = []; const saved = [];
  const env = environment({ request: async (path, options) => {
    calls.push([path, options]);
    return { session: { ...session, instructions: JSON.parse(options.body).instructions } };
  } });
  const subject = renderer(SessionInstructions, { session, onSaved: value => saved.push(value) }, env);
  try {
    await subject.settle();
    const textarea = () => nodes(subject.tree, node => node.type === "textarea")[0];
    assert.equal(textarea().props.value, "");
    textarea().props.onChange({ target: { value: "Use this Session's instructions.\nKeep exact spacing. " } }); await subject.settle();
    await nodes(subject.tree, node => node.type === "form")[0].props.onSubmit({ preventDefault() {} }); await subject.settle();
    assert.equal(calls[0][0], "/api/sessions/session");
    assert.equal(calls[0][1].method, "PATCH");
    assert.deepEqual(JSON.parse(calls[0][1].body), { instructions: "Use this Session's instructions.\nKeep exact spacing. " });
    assert.equal(saved[0].instructions, textarea().props.value);
  } finally { subject.unmount(); }
});

test("a rejected instruction update keeps the draft available to retry", async () => {
  const subject = renderer(SessionInstructions, { session, onSaved: () => assert.fail("rejected update applied") }, environment({ request: async () => { throw new Error("forbidden"); } }));
  try {
    await subject.settle(); nodes(subject.tree, node => node.type === "textarea")[0].props.onChange({ target: { value: "Keep this draft" } }); await subject.settle();
    await nodes(subject.tree, node => node.type === "form")[0].props.onSubmit({ preventDefault() {} }); await subject.settle();
    assert.equal(nodes(subject.tree, node => node.props.role === "alert")[0].props.children, "sessionInstructions.saveFailed");
    assert.equal(nodes(subject.tree, node => node.type === "textarea")[0].props.value, "Keep this draft");
    assert.equal(button(subject.tree, "modelSettings.save").props.disabled, false);
  } finally { subject.unmount(); }
});

test("leaving a Session aborts its instruction update and ignores a late response", async () => {
  const pending = deferred(); const saved = []; let signal;
  const subject = renderer(SessionInstructions, { session, onSaved: value => saved.push(value) }, environment({ request: (_path, options) => { signal = options.signal; return pending.promise; } }));
  await subject.settle(); nodes(subject.tree, node => node.type === "textarea")[0].props.onChange({ target: { value: "Old Session draft" } }); await subject.settle();
  const saving = nodes(subject.tree, node => node.type === "form")[0].props.onSubmit({ preventDefault() {} });
  subject.unmount(); assert.equal(signal.aborted, true);
  pending.resolve({ session: { ...session, instructions: "Old Session draft" } }); await saving;
  assert.deepEqual(saved, []);
});
