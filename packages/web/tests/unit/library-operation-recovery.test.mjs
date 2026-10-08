import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { ApiError, button, deferred, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";
const harness = new URL("./componentHarness.mjs", import.meta.url).href;
const url = source => `data:text/javascript;base64,${Buffer.from(source).toString("base64")}`;
const api = url(`import {context,ApiError} from ${JSON.stringify(harness)}; export {ApiError}; export const apiJson=(...args)=>context.request(...args); export const apiUrl=path=>path; export const jsonOptions=(method,value)=>({method,body:JSON.stringify(value)});`);
const load = subjectLoader({ extraOverrides: [
  ["../api.ts", api], ["../api", api],
  ["react-router", url(`import {context} from ${JSON.stringify(harness)}; export const Link=()=>null; export const useNavigate=()=>context.navigate; export const useRouteLoaderData=id=>id==="authenticated"?{user:context.user}:{workspace:context.workspace,agents:context.agents}; export const useSearchParams=()=>[new URLSearchParams()];`)],
  ["../i18n", url('export const t=key=>key; export const useTranslation=()=>({t}); export const i18n={resolvedLanguage:"en"};')],
  ["../chat/MarkdownContent", url('export const MarkdownContent=()=>null;')],
] });
const { default: LibraryPage } = await import(await load(fileURLToPath(new URL("../../src/routes/LibraryRoute.jsx", import.meta.url))));
function Page() { return LibraryPage().type(); }
const key = 'centaeris.pendingOperation.v1:["user","workspace","createSession"]';
const receipt = id => ({ operationId: id, command: "createSession", status: "accepted", sessionId: "deleted", agentRunId: null, turnId: null });
const metadata = id => ({ operationId: id, fingerprint: "a".repeat(64), assetIds: ["material"], inputChanged: false, receipt: receipt(id) });
const material = id => ({ id, displayName: id, objectKind: "file", contentType: "text/plain", status: "ready", sizeBytes: 20, createdAt: "2026-10-08" });
function fixture(request) {
  const data = new Map([[key, JSON.stringify(metadata("old"))]]);
  globalThis.sessionStorage = { getItem: k => data.get(k) ?? null, setItem: (k, v) => data.set(k, v), removeItem: k => data.delete(k) };
  const calls = [];
  const env = environment({ request: async (path, options) => {
    calls.push([path, options]);
    if (path === "/api/library") return { objects: [material("material"), material("other")] };
    return request(path, options);
  } });
  return { data, calls, env, subject: renderer(Page, {}, env) };
}
async function retry(subject) {
  await subject.settle();
  nodes(subject.tree, n => n.type === "input" && n.props.type === "checkbox")[0].props.onChange(); await subject.settle();
  button(subject.tree, "libraryRoute.startChat").props.onClick(); await new Promise(resolve => setTimeout(resolve, 20)); await subject.settle();
  button(subject.tree, "libraryObjectRoute.retry").props.onClick(); await subject.settle();
}
for (const resource of ["Session", "Agent"]) test(`Library recovery clears a terminal deleted ${resource} operation and allows new materials`, async () => {
  const f = fixture(async (path, options) => {
    if (!options) throw new ApiError("operation_resource_unavailable", 410);
    return { ...receipt(JSON.parse(options.body).operationId), sessionId: "new-session" };
  });
  try {
    await retry(f.subject);
    assert.equal(f.data.has(key), false); assert.equal(nodes(f.subject.tree, n => n.props.role === "alert")[0].props.children[0], "operations.unavailable");
    assert.equal(nodes(f.subject.tree, n => n.type === "input" && n.props.type === "checkbox").length, 2);
    assert.deepEqual(f.env.navigations, []);
    nodes(f.subject.tree, n => n.type === "input" && n.props.type === "checkbox")[1].props.onChange(); await f.subject.settle();
    button(f.subject.tree, "libraryRoute.startChat").props.onClick();
    await new Promise(resolve => setTimeout(resolve, 20)); await f.subject.settle();
    assert.equal(f.calls.filter(([, options]) => options?.method === "POST").length, 1);
    assert.notEqual(JSON.parse(f.data.get(key)).operationId, "old");
  } finally { f.subject.unmount(); }
});
for (const failure of [new TypeError("offline"), new ApiError("unavailable", 503), new ApiError("forbidden", 403), new ApiError("session_not_found", 404), new ApiError("other_terminal", 410)]) test(`Library retains pending for ${failure.message}`, async () => {
  const f = fixture(async () => { throw failure; });
  try { await retry(f.subject); assert.equal(JSON.parse(f.data.get(key)).operationId, "old"); assert.equal(nodes(f.subject.tree, n => n.props.role === "alert")[0].props.children[0], "operations.acceptedReadFailed"); assert.deepEqual(f.env.navigations, []); }
  finally { f.subject.unmount(); }
});
test("late terminal recovery cannot consume a replacement operation", async () => {
  const pending = deferred(); const f = fixture(() => pending.promise);
  try {
    await retry(f.subject); f.data.set(key, JSON.stringify(metadata("new")));
    pending.reject(new ApiError("operation_resource_unavailable", 410)); await f.subject.settle();
    assert.equal(JSON.parse(f.data.get(key)).operationId, "new"); assert.deepEqual(f.env.navigations, []);
  } finally { f.subject.unmount(); }
});
test("late terminal recovery after workspace switch keeps the old scope metadata", async () => {
  const pending = deferred(); const f = fixture(() => pending.promise);
  try {
    await retry(f.subject); f.env.workspace = { ...f.env.workspace, id: "other-workspace" }; await f.subject.settle();
    pending.reject(new ApiError("operation_resource_unavailable", 410)); await f.subject.settle();
    assert.equal(JSON.parse(f.data.get(key)).operationId, "old"); assert.deepEqual(f.env.navigations, []);
  } finally { f.subject.unmount(); }
});

test("material association errors retain the accepted receipt, including a matching terminal code", async () => {
  const f = fixture(async (path, options) => {
    if (path.includes("/operations/")) return receipt("old");
    if (!options) return { session: { id: "deleted", agentId: "agent" } };
    throw new ApiError("operation_resource_unavailable", 410);
  });
  try {
    await retry(f.subject);
    assert.equal(JSON.parse(f.data.get(key)).operationId, "old");
    assert.equal(f.calls.filter(([path, options]) => path.endsWith("/assets") && options?.method === "POST").length, 1);
    assert.deepEqual(f.env.navigations, []);
  } finally { f.subject.unmount(); }
});
test("late terminal recovery after unmount keeps pending metadata", async () => {
  const pending = deferred(); const f = fixture(() => pending.promise);
  await retry(f.subject); f.subject.unmount();
  pending.reject(new ApiError("operation_resource_unavailable", 410)); await Promise.resolve(); await Promise.resolve();
  assert.equal(JSON.parse(f.data.get(key)).operationId, "old"); assert.deepEqual(f.env.navigations, []);
});
test("starting chat with identical pending inputs clears the terminal lookup without resubmitting", async () => {
  const { createHash } = await import("node:crypto");
  const f = fixture(async () => { throw new ApiError("operation_resource_unavailable", 410); });
  const fingerprint = createHash("sha256").update(JSON.stringify({ assetIds: ["material"], input: { agentId: "agent" }, path: "/api/workspaces/workspace/sessions", uploads: [] })).digest("hex");
  f.data.set(key, JSON.stringify({ ...metadata("old"), fingerprint }));
  try {
    await f.subject.settle(); nodes(f.subject.tree, n => n.type === "input" && n.props.type === "checkbox")[0].props.onChange(); await f.subject.settle();
    button(f.subject.tree, "libraryRoute.startChat").props.onClick(); await new Promise(resolve => setTimeout(resolve, 20)); await f.subject.settle();
    assert.equal(f.data.has(key), false); assert.equal(nodes(f.subject.tree, n => n.props.role === "alert")[0].props.children[0], "operations.unavailable"); assert.equal(f.calls.filter(([, options]) => options?.method === "POST").length, 0);
    assert.deepEqual(f.env.navigations, []);
  } finally { f.subject.unmount(); }
});
