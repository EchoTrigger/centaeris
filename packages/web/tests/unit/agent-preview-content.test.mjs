import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { renderToStaticMarkup } from "react-dom/server";
import { ApiError, button, deferred, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";
const { AgentSessionOutputs, AgentMessageFiles, AgentFilePreview, AgentOverviewSessions } = await import(await subjectLoader()(fileURLToPath(new URL("../../src/agent-chat/AgentPreviewContent.tsx", import.meta.url))));
const sha256 = `sha256:${"a".repeat(64)}`;
const file = (id = "artifact", inputRef = null) => ({ inputRef, agentRunId: "own-run", ownerKind: inputRef ? "sourceObject" : "artifact", objectRef: id, sourceVersion: "1", sha256, displayName: `${id}.txt`, contentType: "text/plain", sizeBytes: 12,
  ...Object.fromEntries(["preview", "download"].map(action => [`${action}Url`, `${inputRef ? `/api/agents/agent/messages/reply/files/${inputRef}` : `/api/sessions/work/outputs/${id}`}/${action}?${new URLSearchParams({ sourceVersion: "1", sha256, lang: "zh-CN" })}`])) });
const page = (changes = {}) => ({ schema: "session.preview.v1", sessionId: "work", runFact: null, outputs: [], nextAfterArtifactId: null, hasMore: false, ...changes });
const props = { sessionId: "work", onPreviewFile() {} };
const { AgentOverviewPanel } = await import(await subjectLoader()(fileURLToPath(new URL("../../src/agent-chat/AgentOverviewPanel.tsx", import.meta.url))));

test("pending output metadata does not show a loading line or a premature empty state", async () => {
  const request = deferred();
  const subject = renderer(AgentSessionOutputs, props, environment({ request: () => request.promise }));
  try {
    await subject.settle();
    assert.equal(nodes(subject.tree, node => node.props.role === "status").length, 0);
    assert.equal(nodes(subject.tree, node => node.props.children === "agentChat.noOutputs").length, 0);
  } finally { subject.unmount(); }
});

test("overview header does not request a second coordination run status", async () => {
  const requests = [];
  const env = environment({ request: async path => { requests.push(path); return page({ sessionId: "coord", runFact: { agentRunId: "run", status: "running", sourceType: "hostedRun", eventId: null, createdAtMs: 1 } }); } });
  const subject = renderer(AgentOverviewPanel, { agent: env.agents[0], sessionId: "coord", sessions: [], onPreviewSession() {}, onPreviewFile() {}, onRunState() {}, onClose() {} }, env);
  const children = [];
  try {
    await subject.settle();
    const header = nodes(subject.tree, node => node.type === "header")[0];
    for (const node of nodes(header, item => item.type.name === "AgentRunOverview")) {
      const child = renderer(node.type, node.props, env); children.push(child); await child.settle();
    }
    assert.deepEqual(requests, [], "the coordination run must not duplicate the Work Session indicator");
  } finally { children.forEach(child => child.unmount()); subject.unmount(); }
});

test("overview card has no duplicate close control alongside the page List toggle", async () => {
  const env = environment();
  const subject = renderer(AgentOverviewPanel, { agent: env.agents[0], sessions: [], onPreviewSession() {}, onPreviewFile() {}, onRunState() {}, onClose() {} }, env);
  try { await subject.settle(); assert.equal(button(subject.tree, "agentChat.close"), undefined); }
  finally { subject.unmount(); }
});

test("Outputs renders file names without another Current Run label", async () => {
  const subject = renderer(AgentSessionOutputs, props, environment({ request: async () => page({ outputs: [file()], runFact: { agentRunId: "run", status: "running", sourceType: "hostedRun", eventId: null, createdAtMs: 1 } }) }));
  try {
    await subject.settle();
    const markup = renderToStaticMarkup(subject.tree);
    assert.ok(markup.includes("artifact.txt"));
    assert.equal(markup.includes("agentChat.currentRun"), false);
    assert.equal(markup.includes("agentChat.run.running"), false);
  } finally { subject.unmount(); }
});

test("overview revision refresh retains the loaded output window and removes withdrawn files", async () => {
  let changed = false; const requests = [];
  const first = Array.from({ length: 50 }, (_, index) => file(`artifact-${index}`));
  const list = AgentOverviewSessions({ sessions: [{ sessionId: "work", agentId: "worker", title: "Work", runState: "unknown" }], revision: "first", onPreviewSession() {}, onRunState() {}, onPreviewFile() {} });
  const node = nodes(list, item => item.type.name === "OverviewSession")[0];
  const values = { ...node.props };
  const subject = renderer(node.type, values, environment({ request: async path => {
    requests.push(path);
    const after = new URL(path, "https://fixture.invalid").searchParams.get("afterArtifactId");
    return after ? page({ outputs: [file(changed ? "replacement" : "artifact-50")] })
      : page({ outputs: first, hasMore: true, nextAfterArtifactId: "artifact-49" });
  } }));
  try {
    await subject.settle(); button(subject.tree, "agentChat.showMore").props.onClick(); await subject.settle();
    values.revision = "empty-history-poll"; await subject.settle();
    assert.equal(nodes(subject.tree, item => item.props.className === "agentFileReference").length, 51);
    assert.ok(button(subject.tree, "artifact-50.txt"));
    changed = true; values.revision = "changed-files"; await subject.settle();
    assert.equal(button(subject.tree, "artifact-50.txt"), undefined);
    assert.ok(button(subject.tree, "replacement.txt"));
    assert.equal(requests.length, 6);
  } finally { subject.unmount(); }
});

test("overview uses an authoritative running fact; revision refresh and file denial retain the preview identity boundary", async () => {
  let status = "queued"; const states = []; let opened;
  const session = { sessionId: "work", agentId: "worker", title: "Synthetic work", runState: "unknown" };
  const list = AgentOverviewSessions({ sessions: [session], revision: "first", onPreviewSession() {}, onRunState: (...args) => states.push(args), onPreviewFile: (...args) => { opened = args; } });
  const node = nodes(list, item => item.type.name === "OverviewSession")[0];
  const env = environment({ request: async () => page({ outputs: [file()], runFact: { agentRunId: "run", status, sourceType: status === "completed" ? "sessionEvent" : "hostedRun", eventId: status === "completed" ? "completed-event" : null, createdAtMs: 1 } }) });
  const values = { ...node.props }; const subject = renderer(node.type, values, env);
  try {
    await subject.settle();
    const reference = () => nodes(list, item => item.type.name === "AgentSessionReference")[0];
    assert.equal(reference().props.session.runState, "unknown");
    status = "running"; values.revision = "running"; await subject.settle();
    assert.deepEqual(states.at(-1), ["work", "running"]);
    assert.deepEqual(states.at(-1), ["work", "running"]);
    button(subject.tree, "artifact.txt").props.onClick();
    assert.deepEqual([opened[0].file, opened[1]], [file(), "work"]);
    opened[0].invalidateMetadata(); await subject.settle();
    assert.equal(button(subject.tree, "artifact.txt"), undefined);
    assert.equal(reference().props.session.runState, "unknown");
    status = "completed"; values.revision = "completed"; await subject.settle();
    assert.equal(reference().props.session.runState, "unknown");
    assert.equal(reference().props.session.title, session.title);
  } finally { subject.unmount(); }
});

test("Work Session reference keeps its identity without inferring completion when the Run fact is absent", async () => {
  const states = [];
  const list = AgentOverviewSessions({ sessions: [{ sessionId: "work", agentId: "worker", title: "Work", runState: "unknown" }], onPreviewSession() {}, onPreviewFile() {}, onRunState: (...args) => states.push(args) });
  const node = nodes(list, item => item.type.name === "OverviewSession")[0];
  const subject = renderer(node.type, node.props, environment({ request: async () => page() }));
  try {
    await subject.settle();
    const reference = nodes(list, item => item.type.name === "AgentSessionReference")[0];
    assert.equal(reference.props.session.sessionId, "work");
    assert.equal(reference.props.session.runState, "unknown");
    assert.deepEqual(states.at(-1), ["work", "unknown"]);
  } finally { subject.unmount(); }
});

test("Outputs opens an actual Artifact; transient retry remains a GET and revocation removes old files", async () => {
  let calls = 0; let opened;
  const subject = renderer(AgentSessionOutputs, { ...props, onPreviewFile: (...args) => { opened = args; } }, environment({ request: async (_path, options) => {
    assert.equal(options.method ?? "GET", "GET");
    if (++calls === 1) throw new ApiError("temporary", 503);
    if (calls === 2) return page({ outputs: Array.from({ length: 50 }, (_, index) => file(index ? `artifact-${index}` : "artifact")), hasMore: true, nextAfterArtifactId: "artifact-49" });
    if (calls === 3) throw new ApiError("temporary", 503);
    throw new ApiError("revoked", 404);
  } }));
  await subject.settle(); button(subject.tree, "agentChat.retry").props.onClick(); await subject.settle();
  button(subject.tree, "artifact.txt").props.onClick(); assert.deepEqual([opened[0].file, opened[1]], [file(), "work"]);
  // A current authorization failure on a subsequent read must clear cached rows.
  button(subject.tree, "agentChat.loadMoreOutputs").props.onClick(); await subject.settle();
  button(subject.tree, "agentChat.retry").props.onClick(); await subject.settle();
  assert.equal(button(subject.tree, "artifact.txt"), undefined); assert.equal(calls, 4); subject.unmount();
});

test("Outputs pagination uses the server cursor, appends bounded pages and rejects repeated object identities", async () => {
  let calls = 0;
  const first = Array.from({ length: 50 }, (_, index) => file(`artifact-${index}`));
  const subject = renderer(AgentSessionOutputs, props, environment({ request: async path => {
    if (++calls === 1) return page({ outputs: first, hasMore: true, nextAfterArtifactId: "artifact-49" });
    assert.equal(new URL(path, "https://fixture.invalid").searchParams.get("afterArtifactId"), calls === 2 ? "artifact-49" : "artifact-99");
    if (calls === 2) return page({ outputs: Array.from({ length: 50 }, (_, index) => file(`artifact-${index+50}`)), hasMore: true, nextAfterArtifactId: "artifact-99" });
    return page({ outputs: [file("artifact-0")] });
  } }));
  await subject.settle(); button(subject.tree, "agentChat.loadMoreOutputs").props.onClick(); await subject.settle();
  assert.equal(nodes(subject.tree, node => node.props.className === "agentFileReference").length, 100);
  button(subject.tree, "agentChat.loadMoreOutputs").props.onClick(); await subject.settle();
  assert.ok(nodes(subject.tree, node => node.props.role === "alert").length); assert.equal(nodes(subject.tree, node => node.props.className === "agentFileReference").length, 100); subject.unmount();
});

test("an aborted Outputs request cannot publish data after navigation", async () => {
  const pending = deferred(); let signal;
  const subject = renderer(AgentSessionOutputs, props, environment({ request: (_path, options) => { signal = options.signal; return pending.promise; } }));
  await subject.settle(); subject.unmount(); assert.equal(signal.aborted, true);
  pending.resolve(page({ outputs: [file()] })); await subject.settle();
  assert.equal(button(subject.tree, "artifact.txt"), undefined);
});

test("message file metadata retry is read-only and preserves the original message binding", async () => {
  let calls = 0; let opened;
  const subject = renderer(AgentMessageFiles, { messageId: "reply", binding: { agentId: "agent", sessionId: "coord", agentRunId: "own-run", inputRefs: ["input"] }, onPreviewFile: (...args) => { opened = args; } }, environment({ request: async (path, options) => {
    assert.equal(path, "/api/agents/agent/messages/reply/files"); assert.equal(options.method ?? "GET", "GET");
    if (++calls !== 2) throw new ApiError("not_available", 404);
    return { schema: "agent.message.files.v1", agentId: "agent", sessionId: "coord", messageId: "reply", agentRunId: "own-run", files: [file("source", "input")] };
  } }));
  await subject.settle(); button(subject.tree, "agentChat.retry").props.onClick(); await subject.settle();
  const attachment = button(subject.tree, "source.txt");
  assert.ok(nodes(attachment, node => node.type?.displayName === "FileOutput").length, "message attachment uses Lucide FileOutput");
  attachment.props.onClick(); assert.deepEqual([opened[0].file, opened[1]], [file("source", "input"), "coord"]); subject.unmount();
});

test("preview reload keeps the captured content URL and download retains Artifact identity", async () => {
  const subject = renderer(AgentFilePreview, { file: file() }, environment()); await subject.settle();
  const before = nodes(subject.tree, node => node.type.name === "DocumentPreview")[0];
  assert.equal(nodes(subject.tree, node => node.type === "a")[0].props.href, file().downloadUrl);
  button(subject.tree, "agentChat.retryPreview").props.onClick(); await subject.settle();
  const after = nodes(subject.tree, node => node.type.name === "DocumentPreview")[0];
  assert.equal(after.props.src, before.props.src); assert.notEqual(after.key, before.key); subject.unmount();
});

test("unsupported file types offer the same bound download without creating an embed", async () => {
  const subject = renderer(AgentFilePreview, { file: { ...file(), displayName: "archive.zip", contentType: "application/zip" } }, environment()); await subject.settle();
  assert.equal(nodes(subject.tree, node => node.type.name === "DocumentPreview").length, 0);
  assert.equal(button(subject.tree, "agentChat.retryPreview"), undefined); subject.unmount();
});
