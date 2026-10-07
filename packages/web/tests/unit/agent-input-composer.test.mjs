import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { ApiError, button, deferred, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";

const { AgentInputComposer } = await import(await subjectLoader()(fileURLToPath(new URL("../../src/agent-chat/AgentInputComposer.tsx", import.meta.url))));
const binding = { schema: "agent.coordination_session.v1", agentId: "agent", sessionId: "coord" };
const settings = { schema: "agent.model_settings.v1", agentId: "agent", modelConfigRef: "configured-model", thinkingMode: "low", status: "configured" };
const acceptance = submission => ({ schema: "agent.input.accepted.v1", agentId: "agent", sessionId: "coord", input: { ...submission, schema: undefined, sequence: 1, createdAtMs: 1, read: null } });
function accepted(submission) { const value = acceptance(submission); delete value.input.schema; value.input.attachments = submission.attachmentRefs.map(inputRef => ({ inputRef, displayName: inputRef, contentType: "application/octet-stream" })); delete value.input.attachmentRefs; return value; }
function storage() {
  const data = new Map();
  return { getItem: key => data.get(key) ?? null, setItem: (key, value) => data.set(key, value), removeItem: key => data.delete(key) };
}
const textarea = subject => nodes(subject.tree, node => node.type === "textarea")[0];
async function fixture({ request, pendingStorage = storage(), onAccepted = () => {}, sessionId, scope = { agentId: "agent", workspaceId: "workspace", userId: "user" }, settle = true } = {}) {
  globalThis.sessionStorage = pendingStorage;
  const env = environment({ request: request || (async path => path.endsWith("model-settings") ? { ...settings, agentId: scope.agentId } : { ...binding, agentId: scope.agentId }) });
  const subject = renderer(AgentInputComposer, { ...scope, sessionId, onAccepted, onAuthorityLost() {} }, env);
  if (settle) await subject.settle(); else { subject.render(); subject.flush(); }
  return subject;
}
function enter(subject, text) {
  assert.ok(textarea(subject), "real text input must be present");
  textarea(subject).props.onChange({ target: { value: text } }); subject.render();
}
function send(subject) { return nodes(subject.tree, node => node.type === "form")[0].props.onSubmit({ preventDefault() {} }); }

test("Agent composer exposes local attachments while its unfinished library chooser is absent", async () => {
  const subject = await fixture();
  try {
    assert.equal(button(subject.tree, "agentChat.chooseMaterial"), undefined);
    assert.equal(nodes(subject.tree, node => node.type.name === "AgentAttachmentPicker").length, 0);
    assert.ok(nodes(subject.tree, node => node.type === "input" && node.props.type === "file")[0]);
  } finally { subject.unmount(); }
});

test("lost response and route remount retry the same inputId and exact Unicode body; double submit is one request", async () => {
  const pendingStorage = storage(); const posts = []; const lost = deferred(); let notified = 0;
  const request = async (path, options) => {
    if (options?.method === "POST") { const body = JSON.parse(options.body); posts.push(body); return posts.length === 1 ? lost.promise : accepted(body); }
    return path.endsWith("model-settings") ? settings : binding;
  };
  let subject = await fixture({ request, pendingStorage, onAccepted: () => { notified++; } });
  const exact = "  original\n中文 🐈 e\u0301  "; enter(subject, exact);
  void send(subject); void send(subject); await subject.settle(); assert.equal(posts.length, 1);
  lost.reject(new TypeError("lost response")); await subject.settle();
  assert.equal(textarea(subject).props.value, exact); assert.equal(textarea(subject).props.disabled, true);
  subject.unmount();
  subject = await fixture({ request, pendingStorage, onAccepted: () => { notified++; } });
  assert.equal(textarea(subject).props.value, exact); void send(subject); await subject.settle();
  assert.deepEqual(posts[1], posts[0]); assert.equal(notified, 1);
  assert.equal(textarea(subject).props.value, ""); subject.unmount();
});

test("a foreign coordination binding disables sending and does not guess a Session", async () => {
  const posts = [];
  const subject = await fixture({ sessionId: "coord", request: async (path, options) => {
    if (options?.method === "POST") posts.push(path);
    return path.endsWith("model-settings") ? settings : { ...binding, sessionId: "foreign" };
  } });
  enter(subject, "keep draft"); void send(subject); await subject.settle();
  assert.deepEqual(posts, []); assert.ok(nodes(subject.tree, node => node.props.children === "agentChat.inputBindingLost").length); subject.unmount();
});

test("Agent composer uploads an image as an attachment, preserves it across remount and sends an image-only input", async () => {
  const pendingStorage = storage(); const posts = []; const uploadCalls = [];
  const request = async (path, options) => {
    if (path.endsWith("/uploads")) {
      uploadCalls.push(path); const files = options.body.getAll("files");
      return { libraryObjects: files.map(file => ({ id: "library-image", displayName: file.name })), assets: files.map(file => ({ id: "image", assetKind: "userLibraryObject", displayName: file.name, contentType: file.type, asset: { id: "library-image" } })) };
    }
    if (options?.method === "POST") { const submission = JSON.parse(options.body); posts.push(submission); return accepted(submission); }
    return path.endsWith("model-settings") ? settings : binding;
  };
  let subject = await fixture({ pendingStorage, request });
  const input = nodes(subject.tree, node => node.type === "input" && node.props.type === "file")[0];
  assert.ok(input, "the actual Agent composer must have a file input"); assert.equal(input.props.multiple, true);
  input.props.onChange({ currentTarget: { files: [new File(["image bytes"], "图.png", { type: "image/png" })], value: "chosen" } }); await subject.settle();
  assert.deepEqual(uploadCalls, ["/api/sessions/coord/uploads"]); assert.equal(button(subject.tree, "agentChat.send").props.disabled, false);
  subject.unmount(); subject = await fixture({ pendingStorage, request });
  assert.equal(textarea(subject).props.value, ""); assert.equal(button(subject.tree, "agentChat.send").props.disabled, false);
  void send(subject); await subject.settle();
  assert.equal(posts.length, 1); assert.equal(posts[0].body, ""); assert.deepEqual(posts[0].attachmentRefs, ["image"]);
  assert.equal(button(subject.tree, "agentChat.send").props.disabled, true); subject.unmount();
});

test("upload failure preserves text and ordinary text paste keeps the browser default", async () => {
  const subject = await fixture({ request: async (path, options) => {
    if (path.endsWith("/uploads")) throw new Error("upload unavailable");
    return path.endsWith("model-settings") ? settings : binding;
  } });
  enter(subject, "keep draft"); let prevented = false;
  textarea(subject).props.onPaste({ clipboardData: { files: [], getData: () => "normal text" }, preventDefault: () => { prevented = true; } });
  assert.equal(prevented, false);
  nodes(subject.tree, node => node.type === "input" && node.props.type === "file")[0].props.onChange({ currentTarget: { files: [new File(["file"], "a.txt", { type: "text/plain" })], value: "chosen" } }); await subject.settle();
  assert.equal(textarea(subject).props.value, "keep draft"); assert.ok(nodes(subject.tree, node => node.props.role === "alert" && node.props.children === "agentChat.attachmentUploadError").length); subject.unmount();
});

test("storage failure blocks POST before acceptance and leaves the original draft", async () => {
  const posts = [];
  const pendingStorage = { getItem: () => null, setItem: () => { throw new Error("storage denied"); }, removeItem() {} };
  const subject = await fixture({ pendingStorage, request: async (path, options) => {
    if (options?.method === "POST") posts.push(path);
    return path.endsWith("model-settings") ? settings : binding;
  } });
  enter(subject, "must retain"); void send(subject); await subject.settle();
  assert.deepEqual(posts, []); assert.equal(textarea(subject).props.value, "must retain"); subject.unmount();
});

test("accepted input is complete even if subsequent history refresh fails", async () => {
  let posts = 0;
  const subject = await fixture({ request: async (path, options) => {
    if (options?.method === "POST") { posts++; return accepted(JSON.parse(options.body)); }
    return path.endsWith("model-settings") ? settings : binding;
  }, onAccepted: async () => { throw new Error("history unavailable"); } });
  enter(subject, "accepted once"); void send(subject); await subject.settle();
  assert.equal(posts, 1); assert.equal(textarea(subject).props.value, "");
  assert.equal(button(subject.tree, "agentChat.retrySend"), undefined); subject.unmount();
});

test("unconfigured model keeps submission disabled without exposing configuration links or creating a binding", async () => {
  const calls = [];
  const subject = await fixture({ sessionId: undefined, request: async (path, options) => {
    calls.push([path, options?.method || "GET"]);
    if (path.endsWith("model-settings")) return { ...settings, modelConfigRef: null, thinkingMode: null, status: "unconfigured" };
    throw new ApiError("coordination_session_not_found", 404);
  } });
  const configLinks = nodes(subject.tree, node => node.props.to?.includes("/settings/agents"));
  assert.equal(configLinks.length, 0, "composer does not expose an internal model configuration link");
  assert.equal(button(subject.tree, "agentChat.send").props.disabled, true);
  const status = nodes(subject.tree, node => node.props.role === "status" && node.props.children === "agentChat.modelRequired")[0];
  assert.ok(status, "the reason sending is unavailable must be visible");
  assert.equal(textarea(subject).props["aria-describedby"], status.props.id);
  assert.equal(calls.some(([, method]) => method === "POST"), false); subject.unmount();
});

test("preparing stays silent and an unavailable model explains the disabled send control without losing text", async () => {
  const loading = deferred(); const posts = [];
  const subject = await fixture({ settle: false, request: async (path, options) => {
    if (options?.method === "POST") posts.push(path);
    return path.endsWith("model-settings") ? loading.promise : binding;
  } });
  assert.equal(nodes(subject.tree, node => node.props.role === "status" && node.props.children === "agentChat.inputPreparing").length, 0);
  enter(subject, "keep while preparing");
  assert.equal(button(subject.tree, "agentChat.send").props.disabled, true);
  loading.resolve({ ...settings, status: "unavailable" }); await subject.settle();
  assert.ok(nodes(subject.tree, node => node.props.role === "status" && node.props.children === "agentChat.modelUnavailable").length);
  assert.equal(button(subject.tree, "agentChat.send").props.disabled, true);
  assert.equal(textarea(subject).props.value, "keep while preparing");
  void send(subject); await subject.settle(); assert.deepEqual(posts, []); subject.unmount();
});

test("an ordinary unsent draft survives route remount and reload with exact text", async () => {
  const pendingStorage = storage(); const exact = "  unfinished\n中文 🐈 e\u0301  ";
  let subject = await fixture({ pendingStorage }); enter(subject, exact); subject.unmount();
  subject = await fixture({ pendingStorage }); assert.equal(textarea(subject).props.value, exact);
  assert.equal(textarea(subject).props.disabled, false);
  assert.equal(button(subject.tree, "agentChat.retrySend"), undefined, "an ordinary draft is not an uncertain submission");
  subject.unmount();
});

test("drafts are isolated by user, workspace and agent, and switching back restores each original", async () => {
  const pendingStorage = storage();
  const scopes = [
    { userId: "user", workspaceId: "workspace", agentId: "agent" },
    { userId: "other-user", workspaceId: "workspace", agentId: "agent" },
    { userId: "user", workspaceId: "other-workspace", agentId: "agent" },
    { userId: "user", workspaceId: "workspace", agentId: "other-agent" },
  ];
  for (const [index, scope] of scopes.entries()) {
    const subject = await fixture({ pendingStorage, scope });
    assert.equal(textarea(subject).props.value, "", "a different scope must not inherit another draft");
    enter(subject, `draft ${index}`); subject.unmount();
  }
  for (const [index, scope] of scopes.entries()) {
    const subject = await fixture({ pendingStorage, scope });
    assert.equal(textarea(subject).props.value, `draft ${index}`); subject.unmount();
  }
});

test("clearing or successfully sending an ordinary draft removes it before the next mount", async () => {
  const pendingStorage = storage(); let posts = 0;
  const request = async (path, options) => {
    if (options?.method === "POST") { posts++; return accepted(JSON.parse(options.body)); }
    return path.endsWith("model-settings") ? settings : binding;
  };
  let subject = await fixture({ pendingStorage, request });
  enter(subject, "erase this"); enter(subject, ""); subject.unmount();
  subject = await fixture({ pendingStorage, request }); assert.equal(textarea(subject).props.value, "");
  enter(subject, "send this"); void send(subject); await subject.settle(); subject.unmount();
  subject = await fixture({ pendingStorage, request });
  assert.equal(posts, 1); assert.equal(textarea(subject).props.value, "");
  assert.equal(button(subject.tree, "agentChat.retrySend"), undefined); subject.unmount();
});

test("a confirmed acceptance after route unmount cannot restore the submitted message as a new draft", async () => {
  const pendingStorage = storage(); const response = deferred(); const posts = []; let notified = 0;
  const request = async (path, options) => {
    if (options?.method === "POST") { posts.push(JSON.parse(options.body)); return response.promise; }
    return path.endsWith("model-settings") ? settings : binding;
  };
  let subject = await fixture({ pendingStorage, request, onAccepted: () => notified++ });
  enter(subject, "accepted while away"); void send(subject); await subject.settle();
  assert.equal(posts.length, 1); subject.unmount(); response.resolve(accepted(posts[0]));
  await subject.settle();
  subject = await fixture({ pendingStorage, request });
  assert.equal(textarea(subject).props.value, "", "accepted text must not become an ordinary resend candidate");
  assert.equal(button(subject.tree, "agentChat.retrySend"), undefined); assert.equal(notified, 0); subject.unmount();
});

test("uncertain submitted input takes precedence over an older ordinary draft and acceptance clears both", async () => {
  const pendingStorage = storage(); let subject = await fixture({ pendingStorage });
  enter(subject, "old draft"); subject.unmount();
  pendingStorage.setItem('centaeris.pendingAgentInput.v1:["user","workspace","agent"]', JSON.stringify({
    binding: { agentId: "agent", sessionId: "coord" }, submission: { schema: "agent.input.submit.v1", inputId: "retained", body: "submitted exact" },
  }));
  const posts = []; const request = async (path, options) => {
    if (options?.method === "POST") { const body = JSON.parse(options.body); posts.push(body); return accepted(body); }
    return path.endsWith("model-settings") ? settings : binding;
  };
  subject = await fixture({ pendingStorage, request });
  assert.equal(textarea(subject).props.value, "submitted exact"); assert.equal(textarea(subject).props.disabled, true);
  void send(subject); await subject.settle();
  assert.deepEqual(posts.map(post => [post.inputId, post.body]), [["retained", "submitted exact"]]); subject.unmount();
  subject = await fixture({ pendingStorage, request }); assert.equal(textarea(subject).props.value, ""); subject.unmount();
});

test("draft storage denial keeps editing responsive and explains that navigation cannot preserve it", async () => {
  const pendingStorage = { getItem: () => null, setItem: () => { throw new Error("storage denied"); }, removeItem() {} };
  const subject = await fixture({ pendingStorage });
  assert.doesNotThrow(() => enter(subject, "keep in memory"));
  assert.equal(textarea(subject).props.value, "keep in memory");
  assert.ok(nodes(subject.tree, node => node.props.role === "alert" && node.props.children === "agentChat.draftSaveError").length);
  subject.unmount();
});

test("a blocked sessionStorage getter does not crash the composer and never reaches POST", async context => {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, "sessionStorage");
  context.after(() => Object.defineProperty(globalThis, "sessionStorage", descriptor));
  Object.defineProperty(globalThis, "sessionStorage", { configurable: true, get() { throw new Error("storage blocked"); } });
  const posts = [];
  const env = environment({ request: async (path, options) => {
    if (options?.method === "POST") posts.push(path);
    return path.endsWith("model-settings") ? settings : binding;
  } });
  const subject = renderer(AgentInputComposer, { agentId: "agent", workspaceId: "workspace", userId: "user", onAccepted() {}, onAuthorityLost() {} }, env);
  await assert.doesNotReject(() => subject.settle());
  assert.doesNotThrow(() => enter(subject, "still here"));
  void send(subject); await subject.settle();
  assert.equal(textarea(subject).props.value, "still here"); assert.deepEqual(posts, []); subject.unmount();
});

test("storage access lost after POST leaves the exact pending identity recoverable without a replacement send", async () => {
  const data = new Map(); let blocked = false; const posts = [];
  const pendingStorage = {
    getItem(key) { if (blocked) throw new Error("storage blocked"); return data.get(key) ?? null; },
    setItem(key, value) { data.set(key, value); }, removeItem(key) { data.delete(key); },
  };
  const request = async (path, options) => {
    if (options?.method === "POST") {
      const body = JSON.parse(options.body); posts.push(body);
      if (posts.length === 1) { blocked = true; throw new Error("lost response"); }
      return accepted(body);
    }
    return path.endsWith("model-settings") ? settings : binding;
  };
  const subject = await fixture({ pendingStorage, request });
  enter(subject, "exact pending"); void send(subject); await subject.settle();
  assert.equal(posts.length, 1); assert.equal(textarea(subject).props.value, "exact pending");
  assert.equal(button(subject.tree, "agentChat.send").props.disabled, true);
  blocked = false; button(subject.tree, "agentChat.retry").props.onClick(); await subject.settle();
  assert.equal(textarea(subject).props.disabled, true); void send(subject); await subject.settle();
  assert.deepEqual(posts[1], posts[0]); assert.equal(textarea(subject).props.value, ""); subject.unmount();
});

test("unsupported or malformed draft records do not restore text and expose a visible storage error", async () => {
  for (const raw of ['{"schema":"agent.input.draft.unknown","body":"hidden text"}', '{"schema":"agent.input.draft.v1","body":"hidden text","inputId":"invented"}', "not JSON"]) {
    const pendingStorage = storage();
    pendingStorage.setItem('centaeris.agentInputDraft.v1:["user","workspace","agent"]', raw);
    const subject = await fixture({ pendingStorage });
    assert.equal(textarea(subject).props.value, "");
    assert.ok(nodes(subject.tree, node => node.props.role === "alert" && node.props.children === "agentChat.draftSaveError").length); subject.unmount();
  }
});

test("missing binding is created only on explicit send and its authoritative session is used", async () => {
  const calls = []; let bound = false;
  const subject = await fixture({ sessionId: undefined, request: async (path, options) => {
    calls.push([path, options?.method || "GET", options?.body]);
    if (path.endsWith("model-settings")) return settings;
    if (path.endsWith("coordination-session")) {
      if (options?.method === "POST") { bound = true; assert.equal(options.body, "{}"); return binding; }
      if (!bound) throw new ApiError("coordination_session_not_found", 404);
      return binding;
    }
    assert.ok(bound); return accepted(JSON.parse(options.body));
  } });
  assert.equal(calls.some(([, method]) => method === "POST"), false);
  enter(subject, "explicit send"); void send(subject); await subject.settle();
  assert.equal(calls.filter(([, method]) => method === "POST").length, 2); subject.unmount();
});

test("inputId conflict keeps the original identity/body and never silently creates a replacement", async () => {
  const posts = [];
  const subject = await fixture({ request: async (path, options) => {
    if (options?.method === "POST") { posts.push(JSON.parse(options.body)); throw new ApiError("agent_input_conflict", 409); }
    return path.endsWith("model-settings") ? settings : binding;
  } });
  enter(subject, "original"); void send(subject); await subject.settle(); void send(subject); await subject.settle();
  assert.equal(posts.length, 1); assert.equal(textarea(subject).props.value, "original");
  assert.ok(nodes(subject.tree, node => node.props.children === "agentChat.inputConflict").length); subject.unmount();
});

test("a pending identity cleared by an earlier accepted request is never replaced by a new POST", async () => {
  const pendingStorage = storage(); const posts = [];
  const key = 'centaeris.pendingAgentInput.v1:["user","workspace","agent"]';
  pendingStorage.setItem(key, JSON.stringify({ binding: { agentId: "agent", sessionId: "coord" }, submission: { schema: "agent.input.submit.v1", inputId: "retained", body: "original" } }));
  const subject = await fixture({ pendingStorage, request: async (path, options) => {
    if (options?.method === "POST") { posts.push(JSON.parse(options.body)); return accepted(posts.at(-1)); }
    return path.endsWith("model-settings") ? settings : binding;
  } });
  pendingStorage.removeItem(key); void send(subject); await subject.settle(); void send(subject); await subject.settle();
  assert.deepEqual(posts, []); assert.equal(textarea(subject).props.value, "original"); assert.equal(textarea(subject).props.disabled, true); subject.unmount();
});
