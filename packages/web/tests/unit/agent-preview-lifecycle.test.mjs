import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { renderToStaticMarkup } from "react-dom/server";
import { createWorkspaceTranscriptTransport } from "../../src/chat/transcriptTransport.ts";
import { ApiError, button, deferred, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";

const url = path => fileURLToPath(new URL(path, import.meta.url));
const dataUrl = source => `data:text/javascript;base64,${Buffer.from(source).toString("base64")}`;
const harness = new URL("./componentHarness.mjs", import.meta.url).href;
const loader = subjectLoader({ extraOverrides: [
  ["react", dataUrl(`export * from ${JSON.stringify(import.meta.resolve("react"))}; export {useState,useRef,useMemo,useCallback,useEffect,useLayoutEffect} from ${JSON.stringify(harness)}; export const useId=()=>"preview-tabs";`)],
  ["../chat/TranscriptBlockList", dataUrl("export const TranscriptBlockList=()=>null;")],
  ["../chat/transcriptTransport", dataUrl(`import {context} from ${JSON.stringify(harness)}; export const createWorkspaceTranscriptTransport=()=>context.transport;`)],
] });
const { AgentSessionPreviewShell } = await import(await loader(url("../../src/agent-chat/AgentSessionPreviewShell.tsx")));
const { AgentSessionOutputs } = await import(await loader(url("../../src/agent-chat/AgentPreviewContent.tsx")));
const { AgentSessionActivityPreview } = await import(await loader(url("../../src/agent-chat/AgentSessionActivityPreview.tsx")));

// Activity deactivates and re-creates Effects without discarding component state.
// Browser route tests separately exercise the actual React boundary and scroll.
function deactivate(subject) { subject.unmount(); subject.effects = []; subject.pending.clear(); subject.render(); }

test("Session preview retains its complete conversation when output files are opened and returned", async () => {
  const props = { embedded: true, session: { sessionId: "work", title: "Work", runState: "unknown" }, agentLabel: "Bot",
    labels: { preview: "Preview", path: "Path", close: "Close", loading: "Loading", retry: "Retry", returnToSession: "Return" },
    load: { status: "ready" }, conversation: "Complete conversation", onReturnToSession() {}, onReturn() {}, onClose() {},
  };
  const subject = renderer(AgentSessionPreviewShell, props, environment());
  try {
    await subject.settle(); const markup = renderToStaticMarkup(subject.tree);
    assert.ok(markup.includes("Complete conversation"));
    assert.equal(markup.includes('role="tablist"'), false);
    assert.equal(markup.includes("Open chat"), false);
  } finally { subject.unmount(); }
});

const sha256 = `sha256:${"a".repeat(64)}`;
const file = id => ({ inputRef: null, agentRunId: "run", ownerKind: "artifact", objectRef: id, sourceVersion: "1", sha256, displayName: `${id}.txt`, contentType: "text/plain", sizeBytes: 12,
  ...Object.fromEntries(["preview", "download"].map(action => [`${action}Url`, `/api/sessions/work/outputs/${id}/${action}?${new URLSearchParams({ sourceVersion: "1", sha256, lang: "zh-CN" })}`])),
});
const page = changes => ({ schema: "session.preview.v1", sessionId: "work", runFact: null, outputs: [], nextAfterArtifactId: null, hasMore: false, ...changes });

test("Outputs reactivation reauthorizes all 51 opened files and clears cached names after denial", async () => {
  let denied = false; let pendingRefresh = null; const calls = [];
  const first = Array.from({ length: 50 }, (_, index) => file(`artifact-${index}`));
  const subject = renderer(AgentSessionOutputs, { sessionId: "work", onPreviewFile() {} }, environment({ request: async path => {
    calls.push(path); if (denied) throw new ApiError("denied", 403);
    if (pendingRefresh) { const waiting = pendingRefresh; pendingRefresh = null; await waiting.promise; }
    return new URL(path, "https://fixture.invalid").searchParams.has("afterArtifactId") ? page({ outputs: [file("artifact-50")] })
      : page({ outputs: first, nextAfterArtifactId: "artifact-49", hasMore: true });
  } }));
  try {
    await subject.settle(); button(subject.tree, "agentChat.loadMoreOutputs").props.onClick(); await subject.settle();
    assert.ok(button(subject.tree, "artifact-50.txt"));
    deactivate(subject);
    const waiting = deferred(); pendingRefresh = waiting;
    await subject.settle();
    const files = nodes(subject.tree, node => node.props.className === "agentPreviewFiles")[0];
    assert.equal(files?.props["aria-hidden"], true, "previously authorized names are hidden until the reopened view is reauthorized");
    assert.equal(files?.props.inert, true);
    waiting.resolve(); await subject.settle();
    assert.equal(nodes(subject.tree, node => node.props.className === "agentFileReference").length, 51);
    assert.equal(calls.length, 4, "refresh both authoritative pages rather than collapsing the opened window");
    deactivate(subject); denied = true; await subject.settle();
    assert.equal(nodes(subject.tree, node => node.props.className === "agentFileReference").length, 0);
    assert.ok(button(subject.tree, "agentChat.retry"));
  } finally { subject.unmount(); }
});

const content = inlineContent => ({ inlineContent, sourceRef: null });
const block = (id, sequence) => ({ blockId: `${id}:user`, blockRevision: "1", orderKey: { sourceSequence: String(sequence), ordinal: 0 }, body: { kind: "userText", content: content(id) } });
const transcriptPage = (blocks, olderCursor = null) => ({ schema: "transcript.page.v1", sessionId: "work", projectionVersion: "transcript.projection.v1", projectionGeneration: "generation", sourceHighWater: "8", blocks, olderCursor, hasOlder: olderCursor !== null, resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: "8" }] });

test("Activity reactivation preserves the authoritative opened history window and hides denied cached blocks", async () => {
  let denied = false; const calls = [];
  const transport = createWorkspaceTranscriptTransport({ request: async (path, options) => {
    calls.push(path); assert.equal(options.signal.aborted, false);
    if (denied) throw new ApiError("denied", 403);
    const value = path.includes("active-agent-run") ? { schema: "workspace.transcript.active_agent_run.v1", sessionId: "work", agentRun: null }
      : new URL(path, "https://fixture.invalid").searchParams.has("olderCursor") ? transcriptPage([block("older", 1)])
      : transcriptPage([block("tail", 5)], "older-page");
    return { json: async () => value };
  } });
  const states = [];
  const subject = renderer(AgentSessionActivityPreview, { sessionId: "work", onRunState: (...args) => states.push(args) }, environment({ transport }));
  const reader = () => nodes(subject.tree, node => node.type.name === "TranscriptBlockList")[0];
  try {
    await subject.settle(); reader().props.onLoadOlderHistory(); await subject.settle();
    assert.deepEqual(reader().props.store.getListSnapshot().blockIds, ["older:user", "tail:user"]);
    deactivate(subject); await subject.settle();
    assert.deepEqual(reader().props.store.getListSnapshot().blockIds, ["older:user", "tail:user"]);
    assert.equal(calls.filter(path => path.includes("olderCursor")).length, 2);
    deactivate(subject); denied = true; await subject.settle();
    assert.equal(reader(), undefined, "cached transcript is not displayed after reauthorization fails");
    assert.ok(button(subject.tree, "agentChat.retry"));
    assert.deepEqual(states.at(-1), ["work", "unknown"]);
  } finally { subject.unmount(); }
});
