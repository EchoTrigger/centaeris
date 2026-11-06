import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { readFileSync } from "node:fs";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createJsxSubjectLoader } from "./jsxSubject.mjs";
import { ApiError, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";
import { MODEL_ERROR_KEYS, localizedModelError, modelErrorText, modelFailureDetail } from "../../src/modelErrors.ts";
import { createTranscriptViewStore } from "../../src/chat/transcriptViewStore.ts";

test("local clients share the Web error wording", () => {
  const read = path => JSON.parse(readFileSync(new URL(path, import.meta.url), "utf8"));
  const english = read("../../src/locales/en.json");
  const catalog = read("../../../../assets/run-error-messages.json");
  assert.deepEqual(Object.keys(catalog.messages).sort(), Object.keys(MODEL_ERROR_KEYS).sort());
  for (const [reason, entry] of Object.entries(catalog.messages)) {
    assert.equal(localizedModelError(reason, key => key), entry.translationKey);
    assert.equal(entry.english, english[entry.translationKey]);
  }
  for (const { reason, expected } of read("../../../../tests/fixtures/run-error-display.json")) {
    assert.equal(modelFailureDetail(reason, key => english[key]), expected);
  }
});

const stub = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
const loader = createJsxSubjectLoader(new Map([
  ["../i18n", stub("export const t=key=>key; export const i18n={language:'en'}; export const useTranslation=()=>({t,i18n});")],
  ["./MarkdownContent", stub('import {createElement} from "' + import.meta.resolve("react") + '"; export const MarkdownContent=({text})=>createElement("span",null,text);')],
]));
const { TranscriptBlockRow } = await import(await loader(fileURLToPath(new URL("../../src/chat/TranscriptBlockContent.tsx", import.meta.url))));

function render(kind, sourceType, message = "model_quota_domain_required", reasonType, sourceRef = null) {
  const body = { kind, content: { inlineContent: message, sourceRef },
    ...(kind === "notice" ? { noticeType: "run_boundary", status: "completed" } : kind === "assistantText" ? { status: "completed" } : {}) };
  const block = { blockId: "failure", blockRevision: "1", orderKey: { sourceSequence: "1", ordinal: 0 }, body,
    presentation: sourceType ? { sourceType, agentRunId: "run", observedAtMs: 1000, displayTarget: null, durationMs: null, operation: null, ...(reasonType ? { reasonType } : {}) } : null };
  const store = createTranscriptViewStore();
  store.openTail({ schema: "transcript.page.v1", sessionId: "session", projectionVersion: "transcript.projection.v1", projectionGeneration: "generation",
    sourceHighWater: "1", blocks: [block], olderCursor: null, hasOlder: false, resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: "1" }] });
  return renderToStaticMarkup(createElement(TranscriptBlockRow, { store, blockId: "failure" }));
}

test("historical quota failures use the safe fallback", () => {
  const html = render("notice", "agent_run_failed");
  assert.match(html, /modelErrors.failed/);
  assert.doesNotMatch(html, /model_quota_domain_required/);
});

test("failed and interrupted runs render a plain error strip using the structured reason", () => {
  for (const source of ["agent_run_failed", "agent_run_interrupted"]) {
    const html = render("notice", source, "exceeded retry limit, last status: 429 Too Many Requests", "provider_rate_limited");
    assert.match(html, /workspaceRunError/);
    assert.match(html, /role="status"/);
    assert.match(html, /aria-hidden="true"/);
    assert.match(html, /modelErrors.rateLimited/);
    assert.doesNotMatch(html, /provider_rate_limited|429|exceeded retry/);
    assert.doesNotMatch(html, /workspaceStageSummary|workspaceTerminalAnswer|<details|<button/);
  }
});

test("unclassified failures and referenced diagnostics use the safe fallback", () => {
  for (const reason of ["model_adapter_failed", "prepared_prompt_new_validation_failure", "exceeded retry limit, last status: 429 Too Many Requests"]) {
    const html = render("notice", "agent_run_failed", reason);
    assert.match(html, /modelErrors.failed/);
    assert.ok(!html.includes(reason));
  }
  const referenced = render("notice", "agent_run_failed", null, undefined, { refId: "diagnostic-ref", revision: "1", byteLength: "70000" });
  assert.match(referenced, /modelErrors.failed/);
  assert.doesNotMatch(referenced, /diagnostic-ref|70000/);
});

test("ordinary content never acquires system error strip semantics", () => {
  for (const kind of ["userText", "assistantText"]) {
    assert.doesNotMatch(render(kind, "agent_run_failed", "provider_rate_limited"), /workspaceRunError/);
  }
  assert.doesNotMatch(render("notice", null), /workspaceRunError/);
});
test("failed delivery explains whether the Agent omitted its reply or exhausted context", () => {
  const t = key => key;
  for (const [reason, key] of [
    ["completion_tool_delivery_required", "modelErrors.replyNotDelivered"],
    ["completion_delivery_repair_prompt_too_large", "modelErrors.replyContextLimit"],
  ]) {
    assert.equal(localizedModelError(reason, t), key);
    assert.ok(render("notice", "agent_run_failed", reason).includes(key));
    assert.ok(render("assistantText", "agent_run_failed", reason).includes(reason));
  }
});
test("user, assistant and ordinary notice content is never rewritten as an error code", () => {
  for (const [kind, source] of [["userText", "agent_run_failed"], ["assistantText", "agent_run_failed"], ["notice", null]]) {
    assert.ok(render(kind, source).includes("model_quota_domain_required"));
  }
});

test("configuration reasons have useful product copy and unknown hosted codes have a translated fallback", () => {
  const t = key => key;
  assert.equal(localizedModelError("provider_request_rejected", t), "modelErrors.requestRejected");
  assert.equal(localizedModelError("model_thinking_mode_unsupported", t), "modelErrors.thinkingUnsupported");
  assert.equal(localizedModelError("prepared_prompt_new_validation_failure", t), "modelErrors.failed");
  const error = new ApiError("model_run_failed", 502);
  error.payload = { reasonType: "provider_authentication_failed" };
  assert.equal(modelErrorText(error, t), "modelErrors.authenticationFailed");
});

test("model settings displays the translated provider authentication error from the API", async () => {
  const { default: ModelSettings } = await import(await subjectLoader()(fileURLToPath(new URL("../../src/routes/ModelSettings.jsx", import.meta.url))));
  const error = new ApiError("provider_authentication_failed", 401);
  const subject = renderer(ModelSettings, { onClose() {}, onModelsChanged() {} }, environment({ request: async () => { throw error; } }));
  await subject.settle();
  assert.ok(nodes(subject.tree, node => node.props.children === "modelErrors.authenticationFailed").length);
  subject.unmount();
});
