import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { ApiError, button, deferred, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";

// Real route -> list -> bubble -> metadata cache -> Agent preview -> shared
// content loader effects. Only router/API fixtures, text rendering and PDF paint
// are isolated. HTTP failure classification/propagation is never stubbed.
const loader = subjectLoader({ realDocumentPreview: true, realMessageList: true });
const { AgentChatPageContent } = await import(await loader(fileURLToPath(new URL("../../src/routes/AgentChatRoute.tsx", import.meta.url))));
const { AgentSessionOutputs } = await import(await loader(fileURLToPath(new URL("../../src/agent-chat/AgentPreviewContent.tsx", import.meta.url))));
const { DocumentPreview } = await import(await loader(fileURLToPath(new URL("../../src/components/DocumentPreview.jsx", import.meta.url))));
const dataUrl = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
const officeLoader = subjectLoader({ extraOverrides: [
  ["pdfjs-dist", dataUrl('export const GlobalWorkerOptions={}; export const getDocument=()=>{throw new Error("unexpected PDF paint in HTTP-failure characterization");};')],
  ["pdfjs-dist/build/pdf.worker.min.mjs?worker&url", dataUrl('export default "isolated-worker";')],
] });
const { default: OfficePreview } = await import(await officeLoader(fileURLToPath(new URL("../../src/components/OfficePreview.jsx", import.meta.url))));
const sha256 = `sha256:${"a".repeat(64)}`;
function file(id = "one", changes = {}) {
  const inputRef = `input-${id}`;
  const prefix = `/api/agents/agent/messages/reply/files/${inputRef}`;
  return { inputRef, agentRunId: "own-run", ownerKind: "sourceObject", objectRef: `source-${id}`, sourceVersion: "1", sha256, displayName: `${id}.txt`, contentType: "text/plain", sizeBytes: 12,
    ...Object.fromEntries(["preview", "download"].map(action => [`${action}Url`, `${prefix}/${action}?${new URLSearchParams({ sourceVersion: "1", sha256, lang: "zh-CN" })}`])), ...changes };
}
const shell = subject => nodes(subject.tree, node => node.type.name === "AgentSessionPreviewShell")[0];
const component = (tree, name) => nodes(tree, node => node.type.name === name)[0];
const subjects = [];
function subject(node, env) { const value = renderer(node.type, node.props, env); subjects.push(value); return value; }
async function messagePreview(files = [file()]) {
  const requests = []; let metadataAvailable = true;
  const env = environment({ request: async (path, options) => {
    assert.equal(options?.method ?? "GET", "GET"); requests.push(path);
    if (path.includes("/history")) return { schema: "agent.history.v1", agentId: "agent", sessionId: "coord", items: [{ kind: "message", cursor: "position", message: { id: "reply", agentRunId: "own-run", turnId: "turn", toolCallId: "call", sourceSequence: 1, createdAtMs: 1, body: "Reply", sessionRefs: ["work"], fileRefs: files.map(row => row.inputRef) } }], nextCursor: "tail", newestCursor: "tail", hasMore: false };
    if (path.endsWith("/files")) {
      if (!metadataAvailable) throw new ApiError("preview_not_available", 404);
      return { schema: "agent.message.files.v1", agentId: "agent", sessionId: "coord", messageId: "reply", agentRunId: "own-run", files };
    }
    const id = path.split("/").at(-1);
    return { session: { id, workspaceId: "workspace", agentId: "agent", title: id, status: "active", hasActiveAgentRun: false } };
  } });
  const app = renderer(AgentChatPageContent, { agentId: "agent" }, env); subjects.push(app); await app.settle();
  const list = subject(component(app.tree, "AgentMessageList"), env); await list.settle();
  const bubble = subject(component(list.tree, "AgentMessageBubble"), env); await bubble.settle();
  const metadata = subject(component(bubble.tree, "AgentMessageFiles"), env); await metadata.settle();
  return { app, env, metadata, requests, revokeMetadata: () => { metadataAvailable = false; }, restoreMetadata: () => { metadataAvailable = true; }, async open(id = "one") {
    button(metadata.tree, `${id}.txt`).props.onClick(); await app.settle();
    const preview = subject(shell(app).props.libraryPreview.content, env); await preview.settle(); return preview;
  } };
}
async function consume(preview, env) {
  const node = component(preview.tree, "DocumentPreview") || component(preview.tree, "AgentBinaryPreview");
  assert.ok(node, "Agent content must have an actual request/error consumer");
  const content = subject(node, env); await content.settle(); return content;
}
function cleanup() { while (subjects.length) subjects.pop().unmount(); }

for (const status of [401, 403, 404, 409, 410]) test(`actual content ${status} clears the selected file and its message metadata cache while retaining Session access`, async context => {
  let fetches = 0; let actualStatus = status;
  context.mock.method(globalThis, "fetch", async (_url, options) => {
    assert.equal(options.credentials, "include"); fetches++; return new Response(actualStatus === 200 ? "Current authorized bytes" : "rejected", { status: actualStatus });
  });
  try {
    const fixture = await messagePreview(); fixture.revokeMetadata();
    const preview = await fixture.open(); await consume(preview, fixture.env);
    await fixture.app.settle(); await fixture.metadata.settle();
    assert.equal(shell(fixture.app).props.libraryPreview, undefined);
    assert.equal(shell(fixture.app).props.session.sessionId, "coord", "a file denial does not revoke the authorized Session");
    assert.equal(button(fixture.metadata.tree, "one.txt"), undefined);
    assert.equal(fixture.requests.filter(path => path.endsWith("/files")).length, 1, "handle actual content failure without a preflight");
    assert.equal(fetches, 1);
    button(fixture.metadata.tree, "agentChat.retry").props.onClick(); await fixture.metadata.settle();
    assert.equal(button(fixture.metadata.tree, "one.txt"), undefined);
    if (status === 404) {
      fixture.restoreMetadata(); actualStatus = 200;
      button(fixture.metadata.tree, "agentChat.retry").props.onClick(); await fixture.metadata.settle();
      const restored = await fixture.open(); await consume(restored, fixture.env); await fixture.app.settle();
      assert.equal(shell(fixture.app).props.libraryPreview.title, "one.txt");
    }
  } finally { cleanup(); }
});

test("network and 5xx failures preserve current metadata, selection, download and read-only retry", async context => {
  try {
    for (const failure of [new Response("temporary", { status: 503 }), new TypeError("network disconnected")]) {
      const fetch = context.mock.method(globalThis, "fetch", async () => { if (failure instanceof Error) throw failure; return failure; });
      const fixture = await messagePreview(); const preview = await fixture.open(); const content = await consume(preview, fixture.env);
      await fixture.app.settle(); await fixture.metadata.settle();
      assert.ok(shell(fixture.app).props.libraryPreview); assert.ok(button(fixture.metadata.tree, "one.txt"));
      assert.equal(nodes(preview.tree, node => node.type === "a")[0].props.href, file().downloadUrl);
      assert.ok(button(preview.tree, "agentChat.retryPreview")); assert.ok(nodes(content.tree, node => node.props.role === "alert").length);
      cleanup(); fetch.mock.restore();
    }
  } finally { cleanup(); }
});

for (const target of ["two", "one"]) test(`a late actual failure cannot clear the new selection of ${target === "one" ? "the same file" : "a different object"}`, async context => {
  const pending = deferred();
  let fetches = 0;
  context.mock.method(globalThis, "fetch", async () => ++fetches === 1 ? pending.promise : new Response("current", { headers: { "Content-Type": "text/plain" } }));
  try {
    const fixture = await messagePreview([file(), file("two")]);
    const first = await fixture.open(); const oldContent = await consume(first, fixture.env);
    const second = await fixture.open(target); await consume(second, fixture.env);
    // Let the real old loader's catch run even before its effect cleanup. The
    // owning route must still reject a callback for the old selection.
    pending.resolve(new Response("revoked old file", { status: 404 })); await oldContent.settle();
    await fixture.app.settle(); await fixture.metadata.settle();
    assert.equal(shell(fixture.app).props.libraryPreview.title, `${target}.txt`); assert.ok(button(fixture.metadata.tree, `${target}.txt`));
  } finally { cleanup(); }
});

test("Agent binary previews render the actual fetched bytes and release their object URLs", async context => {
  const created = []; const released = [];
  context.mock.method(URL, "createObjectURL", blob => { created.push(blob); return "blob:actual-response"; });
  context.mock.method(URL, "revokeObjectURL", url => released.push(url));
  try {
    for (const [displayName, contentType, tag] of [["one.png", "image/png", "img"], ["one.pdf", "application/pdf", "iframe"]]) {
      const fetch = context.mock.method(globalThis, "fetch", async (_url, options) => {
        assert.equal(options.cache, "no-store"); return new Response("Actual bytes", { headers: { "Content-Type": contentType } });
      });
      const fixture = await messagePreview([file("one", { displayName, contentType })]);
      button(fixture.metadata.tree, displayName).props.onClick(); await fixture.app.settle();
      const preview = subject(shell(fixture.app).props.libraryPreview.content, fixture.env); await preview.settle(); const content = await consume(preview, fixture.env);
      // Response.blob drains a stream beyond the harness's ordinary microtask
      // turns; yield once to finish the actual byte read before inspecting it.
      await new Promise(resolve => setImmediate(resolve)); await content.settle();
      assert.equal(nodes(content.tree, node => node.type === tag)[0].props.src, "blob:actual-response");
      assert.equal(await created.at(-1).text(), "Actual bytes"); cleanup(); fetch.mock.restore();
    }
    assert.deepEqual(released, ["blob:actual-response", "blob:actual-response"]);
  } finally { cleanup(); }
});

test("a successful non-PDF response remains a content error without revoking file metadata", async context => {
  context.mock.method(globalThis, "fetch", async () => new Response("login page", { headers: { "Content-Type": "text/html" } }));
  try {
    const fixture = await messagePreview([file("one", { displayName: "one.pdf", contentType: "application/pdf" })]);
    button(fixture.metadata.tree, "one.pdf").props.onClick(); await fixture.app.settle();
    const preview = subject(shell(fixture.app).props.libraryPreview.content, fixture.env); await preview.settle(); const content = await consume(preview, fixture.env);
    await fixture.app.settle(); assert.ok(shell(fixture.app).props.libraryPreview);
    assert.equal(nodes(content.tree, node => node.type === "iframe").length, 0);
    assert.ok(nodes(content.tree, node => node.props.role === "alert").length);
  } finally { cleanup(); }
});

test("actual Artifact content failure clears the Outputs cache and selected file", async context => {
  context.mock.method(globalThis, "fetch", async () => new Response("changed version", { status: 409 }));
  try {
    const fixture = await messagePreview();
    const artifact = file("artifact", { inputRef: null, ownerKind: "artifact", objectRef: "artifact" });
    for (const action of ["preview", "download"]) artifact[`${action}Url`] = `/api/sessions/work/outputs/artifact/${action}?${new URLSearchParams({ sourceVersion: "1", sha256, lang: "zh-CN" })}`;
    const request = fixture.env.request;
    fixture.env.request = async (...args) => args[0].includes("/preview?limit=") ? { schema: "session.preview.v1", sessionId: "work", runFact: null, outputs: [artifact], hasMore: false, nextAfterArtifactId: null } : request(...args);
    component(fixture.app.tree, "AgentMessageList").props.onPreviewSession("work"); await fixture.app.settle();
    const outputs = renderer(AgentSessionOutputs, { sessionId: "work", onPreviewFile: component(fixture.app.tree, "AgentOverviewPanel")?.props.onPreviewFile ?? component(fixture.app.tree, "AgentMessageList").props.onPreviewFile }, fixture.env); subjects.push(outputs); await outputs.settle();
    button(outputs.tree, "artifact.txt").props.onClick(); await fixture.app.settle();
    const preview = subject(shell(fixture.app).props.libraryPreview.content, fixture.env); await preview.settle(); await consume(preview, fixture.env);
    await fixture.app.settle(); await outputs.settle();
    assert.equal(shell(fixture.app).props.libraryPreview, undefined); assert.equal(button(outputs.tree, "artifact.txt"), undefined);
    assert.ok(button(outputs.tree, "agentChat.retry"));
  } finally { cleanup(); }
});

test("image and PDF previews consume actual response bytes and propagate authorization errors", async context => {
  context.mock.method(globalThis, "fetch", async () => new Response("revoked", { status: 403 }));
  try {
    for (const [displayName, contentType] of [["one.png", "image/png"], ["one.pdf", "application/pdf"]]) {
      const fixture = await messagePreview([file("one", { displayName, contentType })]);
      button(fixture.metadata.tree, displayName).props.onClick(); await fixture.app.settle();
      const preview = subject(shell(fixture.app).props.libraryPreview.content, fixture.env); await preview.settle(); await consume(preview, fixture.env);
      await fixture.app.settle(); await fixture.metadata.settle();
      assert.equal(shell(fixture.app).props.libraryPreview, undefined); assert.equal(button(fixture.metadata.tree, displayName), undefined); cleanup();
    }
  } finally { cleanup(); }
});

test("real Office HTTP errors propagate through the optional callback to the Agent owner", async context => {
  context.mock.method(globalThis, "fetch", async () => new Response("changed", { status: 409 }));
  const previousObserver = globalThis.ResizeObserver;
  globalThis.ResizeObserver = class { observe() {} disconnect() {} };
  try {
    const fixture = await messagePreview([file("one", { displayName: "one.docx" })]);
    button(fixture.metadata.tree, "one.docx").props.onClick(); await fixture.app.settle();
    const preview = subject(shell(fixture.app).props.libraryPreview.content, fixture.env); await preview.settle();
    const document = await consume(preview, fixture.env);
    const officeProps = document.tree.props.children.props;
    const office = renderer(OfficePreview, officeProps, fixture.env); subjects.push(office); await office.settle();
    const actualLoader = subject(component(office.tree, "PreviewDocument"), fixture.env); await actualLoader.settle();
    await fixture.app.settle(); await fixture.metadata.settle();
    assert.equal(shell(fixture.app).props.libraryPreview, undefined); assert.equal(button(fixture.metadata.tree, "one.docx"), undefined);
  } finally { cleanup(); globalThis.ResizeObserver = previousObserver; }
});

test("shared text preview without an error callback retains its existing generic error presentation", async context => {
  context.mock.method(globalThis, "fetch", async () => new Response("denied", { status: 404 }));
  const content = renderer(DocumentPreview, { src: "/ordinary-session-file", title: "file.txt", contentType: "text/plain" }, environment());
  try { await content.settle(); assert.equal(nodes(content.tree, node => node.props.role === "alert")[0].props.children, "appRoute.unableToLoadThisFile"); } finally { content.unmount(); }
});

test("ordinary Session binary preview retains its existing direct iframe path", async context => {
  const fetch = context.mock.method(globalThis, "fetch", () => { throw new Error("ordinary iframe path must stay unchanged"); });
  const content = renderer(DocumentPreview, { src: "/ordinary-session-pdf", title: "file.pdf", contentType: "application/pdf" }, environment());
  try { await content.settle(); assert.equal(nodes(content.tree, node => node.type === "iframe")[0].props.src, "/ordinary-session-pdf"); assert.equal(fetch.mock.callCount(), 0); } finally { content.unmount(); }
});

test("ordinary Office preview without a callback keeps its generic error and existing reload control", async context => {
  context.mock.method(globalThis, "fetch", async () => new Response("denied", { status: 404 }));
  const previousObserver = globalThis.ResizeObserver;
  globalThis.ResizeObserver = class { observe() {} disconnect() {} };
  try {
    const env = environment(); const office = renderer(OfficePreview, { src: "/ordinary-office", title: "file.docx" }, env); subjects.push(office); await office.settle();
    const content = subject(component(office.tree, "PreviewDocument"), env); await content.settle();
    assert.equal(nodes(content.tree, node => node.props.role === "alert")[0].props.children, "officePreview.error");
    assert.ok(button(office.tree, "officePreview.reload"));
  } finally { cleanup(); globalThis.ResizeObserver = previousObserver; }
});

test("an aborted shared content request does not notify an error for a later selection", async context => {
  const pending = deferred(); const errors = [];
  context.mock.method(globalThis, "fetch", () => pending.promise);
  const content = renderer(DocumentPreview, { src: "/old", title: "file.txt", contentType: "text/plain", onError: error => errors.push(error) }, environment());
  await content.settle(); content.unmount(); pending.resolve(new Response("denied", { status: 404 })); await content.settle(); assert.deepEqual(errors, []);
});
