import "fake-indexeddb/auto";
import assert from "node:assert/strict";
import test from "node:test";
import { createNoteDraftStore, type NoteSaveOperation } from "../../src/library/noteDraftStore.ts";
import { NoteSaveController, type NoteSaveResult, type NoteSaveState } from "../../src/library/noteSaveController.ts";

const document = (markdown: string) => ({ displayName: "title", markdown });
const clock = () => {
  let time = 0;
  const timers = new Map<number, { fn: () => void; due: number }>();
  let id = 0;
  return { now: () => time, setTimeout: (fn: () => void, delay: number) => { timers.set(++id, { fn, due: time + delay }); return id; },
    clearTimeout: (key: number | undefined) => { if (key !== undefined) timers.delete(key); },
    advance: (delta: number) => { time += delta; for (const [key, value] of [...timers]) if (value.due <= time) { timers.delete(key); value.fn(); } },
  };
};
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((accept, fail) => { resolve = accept; reject = fail; });
  return { promise, resolve, reject };
}
async function fixture(send?: (operation: NoteSaveOperation) => Promise<NoteSaveResult>, noteId: string | null = "note") {
  const store = createNoteDraftStore(`note-test-${crypto.randomUUID()}`);
  const calls: NoteSaveOperation[] = [];
  const statuses: string[] = [];
  const timer = clock();
  const options = { userId: "user", noteId, clientDraftId: "draft", editInstanceId: crypto.randomUUID(),
    parentFolderId: "", baseGeneration: noteId ? 1 : 0, document: document("initial"), store, clock: timer,
    onState: (state: NoteSaveState) => statuses.push(state.status),
    send: async (operation: NoteSaveOperation) => {
      calls.push(structuredClone(operation));
      return send ? send(operation) : { saveId: operation.saveId, appliedGeneration: operation.baseGeneration + 1,
        object: { id: operation.noteId || "created", contentGeneration: operation.baseGeneration + 1 } };
    },
  };
  return { store, calls, statuses, timer, options, controller: new NoteSaveController(options) };
}

test("slow request retains only latest pending body and confirms exact local sequence", async () => {
  const first = deferred<NoteSaveResult>();
  let count = 0;
  const f = await fixture(async op => ++count === 1 ? first.promise : {
    saveId: op.saveId, appliedGeneration: op.baseGeneration + 1,
    object: { id: "note", contentGeneration: op.baseGeneration + 1 },
  });
  f.controller.edit(document("B"));
  const inFlight = f.controller.flush();
  while (!f.calls.length) await new Promise(resolve => setImmediate(resolve));
  f.controller.edit(document("C"));
  f.controller.edit(document("D"));
  await f.controller.persist();
  first.resolve({ saveId: f.calls[0].saveId, appliedGeneration: 2, object: { id: "note", contentGeneration: 2 } });
  await inFlight;
  assert.notEqual(f.statuses.at(-1), "saved");
  await f.controller.flush();
  assert.deepEqual(f.calls.map(op => op.document.markdown), ["B", "D"]);
  assert.equal(f.calls[1].baseGeneration, 2);
  assert.equal(f.statuses.at(-1), "saved");
  assert.deepEqual(await f.store.list("user", "note"), []);
  f.controller.dispose();
});

test("unknown create outcome survives reload and retries exact ID before latest body", async () => {
  const f = await fixture(async () => { throw new TypeError("offline"); }, null);
  f.controller.edit(document("B"));
  await f.controller.flush();
  f.controller.edit(document("D"));
  await f.controller.persist();
  const [recovered] = await f.store.list("user", "draft");
  assert.equal(recovered.pending?.saveId, f.calls[0].saveId);
  f.controller.dispose();
  const calls: NoteSaveOperation[] = [];
  const restored = new NoteSaveController({ ...f.options, recovered, editInstanceId: crypto.randomUUID(),
    send: async op => { calls.push(op); return { saveId: op.saveId, appliedGeneration: op.noteId ? 2 : 1,
      object: { id: "created", contentGeneration: op.noteId ? 2 : 1 } }; },
  });
  await restored.flush();
  await restored.flush();
  assert.equal(calls[0].saveId, f.calls[0].saveId);
  assert.equal(calls[0].document.markdown, "B");
  assert.equal(calls[1].noteId, "created");
  assert.equal(calls[1].document.markdown, "D");
  assert.deepEqual(await f.store.list("user", "draft"), []);
  restored.dispose();
});

test("same acknowledged document is skipped and idle never loops", async () => {
  const f = await fixture();
  f.controller.edit(document("initial"));
  await f.controller.flush();
  f.timer.advance(60_000);
  await f.controller.flush();
  assert.equal(f.calls.length, 0);
  f.controller.dispose();
});

test("local persistence failure never claims local save or sends non-durable create", async () => {
  const f = await fixture(undefined, null);
  f.options.store.put = async () => { throw new DOMException("quota", "QuotaExceededError"); };
  f.controller.edit(document("B"));
  await f.controller.flush();
  assert.equal(f.calls.length, 0);
  assert.ok(!f.statuses.includes("local"));
  assert.equal(f.statuses.at(-1), "localError");
  f.controller.dispose();
});

test("disposed editor ignores late response while retaining latest local draft", async () => {
  const first = deferred<NoteSaveResult>();
  const f = await fixture(() => first.promise);
  f.controller.edit(document("B"));
  const request = f.controller.flush();
  while (!f.calls.length) await new Promise(resolve => setImmediate(resolve));
  f.controller.edit(document("D"));
  await f.controller.persist();
  f.controller.dispose();
  const count = f.statuses.length;
  first.resolve({ saveId: f.calls[0].saveId, appliedGeneration: 2, object: { id: "note", contentGeneration: 2 } });
  await request;
  assert.equal(f.statuses.length, count);
  assert.equal((await f.store.list("user", "note"))[0].document.markdown, "D");
});

test("two editor instances retain independent drafts and other users cannot enumerate them", async () => {
  const f = await fixture();
  const second = new NoteSaveController({ ...f.options, editInstanceId: crypto.randomUUID() });
  f.controller.edit(document("A"));
  second.edit(document("B"));
  await Promise.all([f.controller.persist(), second.persist()]);
  assert.equal((await f.store.list("user", "note")).length, 2);
  assert.equal((await f.store.list("other", "note")).length, 0);
  f.controller.dispose(); second.dispose();
});

test("generation conflict pauses autosave and requires explicit rebase", async () => {
  let conflict = true;
  const f = await fixture(async op => {
    if (conflict) throw Object.assign(new Error("note_generation_conflict"), { status: 409 });
    return { saveId: op.saveId, appliedGeneration: 4, object: { id: "note", contentGeneration: 4 } };
  });
  f.controller.edit(document("local"));
  await f.controller.flush();
  assert.equal(f.statuses.at(-1), "conflict");
  f.controller.edit(document("newer local"));
  f.timer.advance(60_000);
  assert.equal(f.calls.length, 1);
  conflict = false;
  f.controller.rebase(3);
  await f.controller.flush();
  assert.equal(f.calls[1].baseGeneration, 3);
  assert.notEqual(f.calls[1].saveId, f.calls[0].saveId);
  f.controller.dispose();
});

for (const competingUpdate of [false, true]) {
  test(`replayed historical success requires a fresh confirmed resubmission${competingUpdate ? " and rejects a newer competing head" : ""}`, async () => {
    const f = await fixture(async () => { throw new TypeError("response lost"); });
    f.controller.edit(document("B"));
    await f.controller.flush();
    const [recovered] = await f.store.list("user", "note");
    f.controller.dispose();
    const calls: NoteSaveOperation[] = [];
    const response = deferred<NoteSaveResult>();
    let serverBody = "C";
    const restored = new NoteSaveController({ ...f.options, baseGeneration: 3, document: document(serverBody),
      recovered, editInstanceId: crypto.randomUUID(), send: async operation => {
        calls.push(structuredClone(operation));
        if (calls.length === 1) return { saveId: operation.saveId, appliedGeneration: 2,
          object: { id: "note", contentGeneration: 2 } };
        const result = await response.promise;
        serverBody = operation.document.markdown;
        return result;
      },
    });
    await restored.flush();
    assert.equal(restored.state.status, "conflict");
    assert.equal(calls[0].saveId, recovered.pending?.saveId);
    assert.ok((await f.store.list("user", "note")).some(record => record.key === recovered.key));
    restored.rebase(3);
    const saving = restored.flush();
    for (let attempt = 0; attempt < 12; attempt++) await new Promise(resolve => setImmediate(resolve));
    assert.equal(calls.length, 2, "explicit resubmission must send a new operation even when B was saved historically");
    assert.notEqual(calls[1].saveId, calls[0].saveId);
    assert.equal(calls[1].baseGeneration, 3);
    assert.equal(calls[1].document.markdown, "B");
    assert.equal(restored.state.status, "syncing");
    assert.equal(serverBody, "C");
    const durable = await f.store.list("user", "note");
    assert.ok(durable.some(record => record.key === recovered.key));
    assert.ok(durable.some(record => record.pending?.saveId === calls[1].saveId));
    if (competingUpdate) {
      serverBody = "D";
      response.reject(Object.assign(new Error("note_generation_conflict"), { status: 409 }));
      await saving;
      assert.equal(restored.state.status, "conflict");
      assert.equal(serverBody, "D");
      assert.ok((await f.store.list("user", "note")).some(record => record.document.markdown === "B"));
    } else {
      response.resolve({ saveId: calls[1].saveId, appliedGeneration: 4, object: { id: "note", contentGeneration: 4 } });
      await saving;
      assert.equal(restored.state.status, "saved");
      assert.equal(serverBody, "B");
      assert.deepEqual(await f.store.list("user", "note"), []);
    }
    restored.dispose();
  });
}

test("explicit resubmission sends the chosen body even when it matches an earlier acknowledged head", async () => {
  let conflict = true;
  const f = await fixture(async operation => {
    if (conflict) throw Object.assign(new Error("note_generation_conflict"), { status: 409 });
    return { saveId: operation.saveId, appliedGeneration: 4, object: { id: "note", contentGeneration: 4 } };
  });
  f.controller.edit(document("B"));
  await f.controller.flush();
  f.controller.edit(document("initial"));
  f.controller.rebase(3);
  conflict = false;
  await f.controller.flush();
  assert.equal(f.calls.length, 2);
  assert.equal(f.calls[1].baseGeneration, 3);
  assert.equal(f.calls[1].document.markdown, "initial");
  assert.notEqual(f.calls[1].saveId, f.calls[0].saveId);
  assert.equal(f.controller.state.status, "saved");
  f.controller.dispose();
});

test("editing during acknowledged draft cleanup cannot report newer text as saved", async () => {
  const f = await fixture();
  const entered = deferred<void>(); const resume = deferred<void>();
  const remove = f.store.deleteIfSequence;
  f.store.deleteIfSequence = async (key, sequence) => { entered.resolve(); await resume.promise; await remove(key, sequence); };
  f.controller.edit(document("B"));
  const saving = f.controller.flush();
  await entered.promise;
  f.controller.edit(document("D"));
  await f.controller.persist();
  resume.resolve();
  await saving;
  assert.notEqual(f.statuses.at(-1), "saved");
  assert.equal((await f.store.list("user", "note"))[0].document.markdown, "D");
  f.controller.dispose();
});

test("editing during acknowledgment persistence cannot claim latest text is locally durable", async () => {
  const response = deferred<NoteSaveResult>();
  const f = await fixture(() => response.promise);
  f.controller.edit(document("B"));
  const saving = f.controller.flush();
  while (!f.calls.length) await new Promise(resolve => setImmediate(resolve));
  f.controller.edit(document("D"));
  await f.controller.persist();
  const entered = deferred<void>(); const resume = deferred<void>();
  const put = f.store.put;
  f.store.put = async record => {
    if (record.baseGeneration === 2) { entered.resolve(); await resume.promise; }
    await put(record);
  };
  response.resolve({ saveId: f.calls[0].saveId, appliedGeneration: 2, object: { id: "note", contentGeneration: 2 } });
  await entered.promise;
  f.controller.edit(document("E"));
  resume.resolve();
  await saving;
  assert.equal(f.statuses.at(-1), "unsaved");
  await f.controller.persist();
  assert.equal(f.statuses.at(-1), "local");
  f.controller.dispose();
});

test("automatic saves honor minimum interval and explicit save bypasses it", async () => {
  const f = await fixture();
  f.controller.edit(document("B"));
  f.timer.advance(1999);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(f.calls.length, 0);
  f.timer.advance(1);
  await f.controller.flush();
  f.controller.edit(document("C"));
  f.timer.advance(4999);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(f.calls.length, 1);
  await f.controller.flush();
  assert.equal(f.calls.length, 2);
  f.controller.dispose();
});

test("copied new draft can be committed without an extra edit", async () => {
  const f = await fixture(undefined, null);
  assert.equal(f.controller.state.status, "unsaved");
  await f.controller.flush();
  assert.equal(f.calls.length, 1);
  assert.equal(f.calls[0].noteId, null);
  assert.equal(f.controller.state.draft.noteId, "created");
  f.controller.dispose();
});

test("slow local storage coalesces intermediate full-body writes", async () => {
  const f = await fixture();
  const entered = deferred<void>(); const resume = deferred<void>();
  const writes: string[] = [];
  const put = f.store.put;
  f.store.put = async record => {
    writes.push(record.document.markdown);
    if (writes.length === 1) { entered.resolve(); await resume.promise; }
    await put(record);
  };
  f.controller.edit(document("B")); const first = f.controller.persist();
  await entered.promise;
  f.controller.edit(document("C")); const second = f.controller.persist();
  f.controller.edit(document("D")); const third = f.controller.persist();
  resume.resolve();
  await Promise.all([first, second, third]);
  assert.deepEqual(writes, ["B", "D"]);
  assert.equal((await f.store.list("user", "note"))[0].document.markdown, "D");
  f.controller.dispose();
});

test("unsupported local draft fields are rejected without replacing the recoverable record", async () => {
  const f = await fixture();
  f.controller.edit(document("B")); await f.controller.persist();
  const [record] = await f.store.list("user", "note");
  const unsupported = { ...record, legacyBody: "wrong" };
  await assert.rejects(f.store.put(unsupported), /note_draft_schema_invalid/);
  assert.equal((await f.store.list("user", "note"))[0].document.markdown, "B");
  f.controller.dispose();
});

test("navigation stays unsafe until latest draft is durable, including quota failures", async () => {
  const f = await fixture();
  assert.equal(f.controller.requiresDraftPersistence, false);
  f.controller.edit(document("B"));
  assert.equal(f.controller.requiresDraftPersistence, true);
  await f.controller.persist();
  assert.equal(f.controller.requiresDraftPersistence, false);
  f.store.put = async () => { throw new DOMException("quota", "QuotaExceededError"); };
  f.controller.edit(document("C"));
  await assert.rejects(f.controller.persist());
  assert.equal(f.controller.requiresDraftPersistence, true);
  f.controller.dispose();
});
