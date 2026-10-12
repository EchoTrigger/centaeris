import "fake-indexeddb/auto";
import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { ApiError, button, deferred, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";
import { createNoteDraftStore } from "../../src/library/noteDraftStore.ts";
const harness = new URL("./componentHarness.mjs", import.meta.url).href;
const url = source => `data:text/javascript;base64,${Buffer.from(source).toString("base64")}`;
const load = subjectLoader({ extraOverrides: [
  ["../i18n", url('export const t=key=>key; export const useTranslation=()=>({t}); export const i18n={language:"en"};')],
  ["react-router", url(`import {context} from ${JSON.stringify(harness)}; export const useBlocker=()=>context.blocker; export const useNavigate=()=>context.navigate; export const useLocation=()=>context.location; export const useParams=()=>({libraryObjectId:context.noteId}); export const useRouteLoaderData=id=>id==="authenticated"?{user:context.user}:{workspace:context.workspace}; export const useSearchParams=()=>[new URLSearchParams(context.location.search)];`)],
  ["../components/SpreadsheetPreview", url("export default ()=>null;")],
] });
const { default: NotePage } = await import(await load(fileURLToPath(new URL("../../src/routes/LibraryObjectRoute.jsx", import.meta.url))));
function Page() { return NotePage().props.children.type(); }
function fixture() {
  const timers = new Map(); let id = 0;
  globalThis.window = { setTimeout: (fn, delay) => { timers.set(++id, { fn, delay }); return id; }, clearTimeout: key => timers.delete(key), addEventListener() {}, removeEventListener() {} };
  const calls = [];
  const item = { id: "editor-note", displayName: "Filename title", objectKind: "note", contentType: "text/markdown", contentGeneration: 1, status: "ready" };
  const env = environment({ noteId: item.id, user: { id: crypto.randomUUID() }, location: { pathname: `/w/workspace/library/${item.id}`, search: "", state: null },
    request: async (path, options) => {
      calls.push([path, options]);
      if (options?.method === "PUT") throw new ApiError("offline", 0);
      return path.endsWith("/note") ? { object: item, markdown: "# Body heading\n\noriginal body" } : { object: item };
    },
  });
  const subject = renderer(Page, {}, env);
  return { subject, calls, timers, env, item, store: createNoteDraftStore() };
}
async function settled(subject) {
  for (let i = 0; i < 12; i++) { await subject.settle(); await new Promise(resolve => setImmediate(resolve)); }
}
const editor = subject => nodes(subject.tree, n => n.type === "textarea" && !n.props.readOnly)[0];
const saveStatus = subject => nodes(subject.tree, node => node.type === "span" && node.props.role === "status")[0].props.children;
async function seedRecovery(f, document, baseGeneration = 1) {
  const userId = f.env.user.id;
  const clientDraftId = crypto.randomUUID();
  const editInstanceId = crypto.randomUUID();
  const record = { schema: 1, key: JSON.stringify([userId, clientDraftId, editInstanceId]), userId, clientDraftId,
    editInstanceId, noteId: f.item.id, parentFolderId: "", baseGeneration, sequence: 1, document, pending: null, updatedAt: Date.now() };
  await f.store.put(record);
  return record;
}

test("title is independent and opening preserves the entire Markdown body", async () => {
  const f = fixture();
  await settled(f.subject);
  assert.equal(editor(f.subject).props.value, "# Body heading\n\noriginal body");
  f.subject.unmount();
});

test("failed save retry sends the draft again and never reloads it over local edits", async () => {
  const f = fixture();
  await settled(f.subject);
  editor(f.subject).props.onChange({ target: { value: "local changes" } });
  await f.subject.settle();
  const save = button(f.subject.tree, "libraryObjectRoute.saveNow");
  if (save) save.props.onClick();
  else [...f.timers.values()].find(value => value.delay === 600).fn();
  await settled(f.subject);
  const getCount = f.calls.filter(([, options]) => !options?.method).length;
  button(f.subject.tree, "libraryObjectRoute.retry").props.onClick();
  await settled(f.subject);
  assert.equal(f.calls.filter(([, options]) => !options?.method).length, getCount);
  assert.equal(editor(f.subject).props.value, "local changes");
  assert.equal(f.calls.filter(([, options]) => options?.method === "PUT").length, 2);
  f.subject.unmount();
});

test("switching notes isolates a late save response from the new editor", async () => {
  const f = fixture();
  const pending = deferred();
  const request = f.env.request;
  let savePayload;
  f.env.request = async (path, options) => {
    if (options?.method === "PUT") { savePayload = JSON.parse(options.body); return pending.promise; }
    if (path.includes("other-note")) return { object: { id: "other-note", objectKind: "note", displayName: "Other", contentType: "text/markdown", contentGeneration: 4 }, markdown: "other body" };
    return request(path, options);
  };
  await settled(f.subject);
  editor(f.subject).props.onChange({ target: { value: "saved first body" } });
  await f.subject.settle();
  button(f.subject.tree, "libraryObjectRoute.saveNow").props.onClick();
  await settled(f.subject);
  f.env.noteId = "other-note";
  await settled(f.subject);
  pending.resolve({ saveId: savePayload.saveId, appliedGeneration: 2,
    object: { id: "editor-note", objectKind: "note", displayName: "Old", contentGeneration: 2 } });
  await settled(f.subject);
  assert.equal(editor(f.subject).props.value, "other body");
  assert.equal(nodes(f.subject.tree, node => node.type === "button" && node.props.children === "Old").length, 0);
  f.subject.unmount();
});

test("mismatched note identity is rejected before enabling the editor", async () => {
  const f = fixture();
  const request = f.env.request;
  f.env.request = async (...args) => { const value = await request(...args); return { ...value, object: { ...value.object, id: "wrong-note" } }; };
  await settled(f.subject);
  assert.equal(editor(f.subject), undefined);
  assert.ok(nodes(f.subject.tree, node => node.props.role === "alert").length > 0);
  f.subject.unmount();
});

for (const renameFirst of [false, true]) {
  test(`recovering after editing uses the confirmed server body${renameFirst ? " and the confirmed renamed title" : ""}`, async () => {
    const f = fixture();
    const displayName = renameFirst ? "Renamed on server" : f.item.displayName;
    const recovered = await seedRecovery(f, { displayName, markdown: "recovered B" }, renameFirst ? 2 : 1);
    const pending = deferred();
    const saves = [];
    const request = f.env.request;
    f.env.request = async (path, options) => {
      if (options?.method !== "PUT") return request(path, options);
      const payload = JSON.parse(options.body);
      saves.push(payload);
      if (renameFirst && saves.length === 1) return { saveId: payload.saveId, appliedGeneration: 2,
        object: { ...f.item, displayName, contentGeneration: 2 } };
      return pending.promise;
    };
    await settled(f.subject);
    if (renameFirst) {
      button(f.subject.tree, "libraryObjectRoute.renameNote").props.onClick();
      await settled(f.subject);
      nodes(f.subject.tree, node => node.type === "input" && node.props["aria-label"] === "libraryObjectRoute.noteTitle")[0]
        .props.onChange({ target: { value: displayName } });
      await settled(f.subject);
      button(f.subject.tree, "libraryObjectRoute.saveNow").props.onClick();
      await settled(f.subject);
      assert.equal(saveStatus(f.subject), "libraryObjectRoute.saveState.saved");
    }
    editor(f.subject).props.onChange({ target: { value: recovered.document.markdown } });
    await settled(f.subject);
    button(f.subject.tree, "libraryObjectRoute.recoverDraft").props.onClick();
    await settled(f.subject);
    assert.equal(saves.length, renameFirst ? 2 : 1, "the editable body is not proof that the recovery document was saved");
    assert.equal(saves.at(-1).markdown, recovered.document.markdown);
    assert.equal(saves.at(-1).displayName, displayName);
    assert.equal(saves.at(-1).baseGeneration, renameFirst ? 2 : 1);
    assert.equal(saveStatus(f.subject), "libraryObjectRoute.saveState.syncing");
    assert.ok((await f.store.list(f.env.user.id, f.item.id)).some(record => record.key === recovered.key));
    const generation = renameFirst ? 3 : 2;
    pending.resolve({ saveId: saves.at(-1).saveId, appliedGeneration: generation,
      object: { ...f.item, displayName, contentGeneration: generation } });
    await settled(f.subject);
    assert.equal(saveStatus(f.subject), "libraryObjectRoute.saveState.saved");
    assert.ok(!(await f.store.list(f.env.user.id, f.item.id)).some(record => record.key === recovered.key));
    f.subject.unmount();
  });
}

test("recovering during an older save serializes requests and a late receipt cannot confirm the recovery document", async () => {
  const f = fixture();
  const recovered = await seedRecovery(f, { displayName: "Recovered title", markdown: "recovered D" });
  const earlier = deferred();
  const final = deferred();
  const saves = [];
  let generation = 1;
  let remote = { displayName: f.item.displayName, markdown: "# Body heading\n\noriginal body" };
  const request = f.env.request;
  f.env.request = async (path, options) => {
    if (options?.method !== "PUT") return path.endsWith("/note")
      ? { object: { ...f.item, displayName: remote.displayName, contentGeneration: generation }, markdown: remote.markdown }
      : request(path, options);
    const payload = JSON.parse(options.body);
    saves.push(payload);
    if (saves.length === 1) return earlier.promise;
    if (payload.baseGeneration !== generation) throw new ApiError("note_generation_conflict", 409);
    return final.promise;
  };
  await settled(f.subject);
  editor(f.subject).props.onChange({ target: { value: "older in-flight B" } });
  await settled(f.subject);
  button(f.subject.tree, "libraryObjectRoute.saveNow").props.onClick();
  await settled(f.subject);
  assert.equal(saves.length, 1);
  button(f.subject.tree, "libraryObjectRoute.recoverDraft").props.onClick();
  await settled(f.subject);
  assert.equal(editor(f.subject).props.value, "recovered D");
  assert.equal(saves.length, 1, "recovery must retain the single in-flight request boundary");
  assert.ok((await f.store.list(f.env.user.id, f.item.id)).some(record => record.key === recovered.key));
  generation = 2;
  remote = { displayName: f.item.displayName, markdown: "older in-flight B" };
  earlier.resolve({ saveId: saves[0].saveId, appliedGeneration: generation,
    object: { ...f.item, contentGeneration: generation } });
  await settled(f.subject);
  assert.equal(saves.length, 2);
  assert.equal(editor(f.subject).props.value, "recovered D");
  assert.equal(saveStatus(f.subject), "libraryObjectRoute.saveState.conflict");
  assert.ok((await f.store.list(f.env.user.id, f.item.id)).some(record => record.key === recovered.key));
  button(f.subject.tree, "libraryObjectRoute.compare").props.onClick();
  await settled(f.subject);
  button(f.subject.tree, "libraryObjectRoute.resubmit").props.onClick();
  await settled(f.subject);
  assert.equal(saves.length, 3);
  assert.equal(saves[2].baseGeneration, 2);
  assert.equal(saves[2].markdown, "recovered D");
  assert.equal(saves[2].displayName, "Recovered title");
  final.resolve({ saveId: saves[2].saveId, appliedGeneration: 3,
    object: { ...f.item, displayName: "Recovered title", contentGeneration: 3 } });
  await settled(f.subject);
  assert.equal(saveStatus(f.subject), "libraryObjectRoute.saveState.saved");
  assert.equal(editor(f.subject).props.value, "recovered D");
  f.subject.unmount();
});

for (const competingUpdate of [false, true]) {
  test(`historical receipt recovery requires an explicit new save through the conflict UI${competingUpdate ? " and preserves another conflict" : ""}`, async () => {
    const f = fixture();
    const recovered = await seedRecovery(f, { displayName: "Recovered B", markdown: "response-lost B" });
    recovered.pending = { saveId: "b".repeat(32), sequence: recovered.sequence, noteId: recovered.noteId,
      baseGeneration: recovered.baseGeneration, document: recovered.document, parentFolderId: "" };
    await f.store.put(recovered);
    const pending = deferred();
    const saves = [];
    let serverBody = "newer C";
    f.env.request = async (path, options) => {
      if (options?.method !== "PUT") return { object: { ...f.item, displayName: "Newer C", contentGeneration: 3 }, markdown: serverBody };
      const payload = JSON.parse(options.body);
      saves.push(payload);
      if (payload.saveId === recovered.pending.saveId) return { saveId: payload.saveId, appliedGeneration: 2,
        object: { ...f.item, displayName: "Recovered B", contentGeneration: 2 } };
      const result = await pending.promise;
      serverBody = payload.markdown;
      return result;
    };
    await settled(f.subject);
    button(f.subject.tree, "libraryObjectRoute.recoverDraft").props.onClick();
    await settled(f.subject);
    assert.equal(saveStatus(f.subject), "libraryObjectRoute.saveState.conflict");
    button(f.subject.tree, "libraryObjectRoute.compare").props.onClick();
    await settled(f.subject);
    assert.deepEqual(nodes(f.subject.tree, node => node.type === "textarea" && node.props.readOnly).map(node => node.props.value), ["response-lost B", "newer C"]);
    button(f.subject.tree, "libraryObjectRoute.resubmit").props.onClick();
    await settled(f.subject);
    assert.equal(saves.length, 2);
    assert.notEqual(saves[1].saveId, saves[0].saveId);
    assert.equal(saves[1].baseGeneration, 3);
    assert.equal(saves[1].markdown, "response-lost B");
    assert.equal(saves[1].displayName, "Recovered B");
    assert.equal(serverBody, "newer C");
    assert.equal(saveStatus(f.subject), "libraryObjectRoute.saveState.syncing");
    assert.ok((await f.store.list(f.env.user.id, f.item.id)).some(record => record.key === recovered.key));
    if (competingUpdate) {
      serverBody = "competing D";
      pending.reject(new ApiError("note_generation_conflict", 409));
      await settled(f.subject);
      assert.equal(saveStatus(f.subject), "libraryObjectRoute.saveState.conflict");
      assert.equal(serverBody, "competing D");
      assert.ok((await f.store.list(f.env.user.id, f.item.id)).some(record => record.key === recovered.key));
    } else {
      pending.resolve({ saveId: saves[1].saveId, appliedGeneration: 4,
        object: { ...f.item, displayName: "Recovered B", contentGeneration: 4 } });
      await settled(f.subject);
      assert.equal(saveStatus(f.subject), "libraryObjectRoute.saveState.saved");
      assert.equal(serverBody, "response-lost B");
      assert.deepEqual(await f.store.list(f.env.user.id, f.item.id), []);
    }
    f.subject.unmount();
  });
}
