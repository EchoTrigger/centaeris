import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { button, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";

globalThis.document = { addEventListener() {}, removeEventListener() {} };
const module = source => `data:text/javascript;base64,${Buffer.from(source).toString("base64")}`;
const load = subjectLoader({ extraOverrides: [
  ["../i18n", module('export const t=key=>key === "workspaceResources.environmentVersionHint" ? "Existing environments keep their mounted package version. Create a new environment or conversation after updating to use the new version." : key; export const useTranslation=()=>({t});')],
  ["../components/ConfirmDialog", module("export const ConfirmDialog=()=>null;")],
  ["../components/ProviderLogo", module("export const ProviderLogo=()=>null;")],
] });
const { default: ModelSettings } = await import(await load(fileURLToPath(new URL("../../src/routes/ModelSettings.jsx", import.meta.url))));
const { default: PluginSettings } = await import(await load(fileURLToPath(new URL("../../src/routes/PluginSettings.jsx", import.meta.url))));
const provider = { id: "local-provider", displayName: "Local provider", api: "openai-completions", apiBase: "https://example.test", credentialVersion: 2, scope: "workspace", writable: true };
const plugin = { name: "example", displayName: "Example", packageDigest: "digest", enabled: true, errors: [], capabilities: ["skills", "mcp"], hooks: [], scope: "workspace", writable: true, mcpServers: [{ id: "service", transport: { type: "streamableHttp" }, auth: { type: "bearer", credentialRef: "service-key" }, tools: [] }] };
function modelEnvironment(catalog = provider) {
  const requests = [];
  const env = environment({ workspace: { id: "one", role: "admin" }, request: async (url, options) => {
    requests.push([url, options]);
    if (url.endsWith("/model-providers")) return { providers: [catalog] };
    if (url.endsWith("/models")) return { models: [] };
    if (url.endsWith("/model-provider-templates")) return { templates: [] };
    return { provider: catalog };
  } });
  return { env, requests };
}
test("workspace administrator loads scoped model metadata and replaces only a newly supplied key", async () => {
  const { env, requests } = modelEnvironment();
  const subject = renderer(ModelSettings, { workspace: env.workspace, onClose() {}, onModelsChanged: async () => {} }, env);
  await subject.settle();
  assert.deepEqual(requests.slice(0, 3).map(([url]) => url).sort(), ["/api/workspaces/one/model-provider-templates", "/api/workspaces/one/model-providers", "/api/workspaces/one/models"]);
  button(subject.tree, provider.displayName).props.onClick(); await subject.settle();
  const key = nodes(subject.tree, node => node.type === "input" && node.props.name === "providerApiKey")[0];
  assert.equal(key.props.value, "");
  assert.equal(key.props.type, "password");
  assert.equal(button(subject.tree, "modelSettings.showOrHideApiKey"), undefined);
  key.props.onChange({ target: { value: "new-secret" } }); await subject.settle();
  button(subject.tree, "modelSettings.save").props.onClick(); await subject.settle();
  assert.equal(requests.find(([url]) => url.endsWith("/credential/rotate"))[0], "/api/workspaces/one/model-providers/local-provider/credential/rotate");
  assert.deepEqual(JSON.parse(requests.find(([url]) => url.endsWith("/credential/rotate"))[1].body), { secret: "new-secret" });
  assert.equal(nodes(subject.tree, node => node.type === "input" && node.props.name === "providerApiKey")[0].props.value, "");
  subject.unmount();
});
test("model probe shows upstream HTTP status on success and failure", async () => {
  const env = environment({ workspace: { id: "one", role: "admin" }, request: async (url) => {
    if (url.endsWith("/model-providers")) return { providers: [provider] };
    if (url.endsWith("/model-provider-templates")) return { templates: [] };
    if (url.endsWith("/models")) return { models: [
      { id: "one-model", displayName: "One model", providerId: provider.id, scope: "workspace", writable: true },
      { id: "busy-model", displayName: "Busy model", providerId: provider.id, scope: "workspace", writable: true },
    ] };
    if (url.endsWith("/one-model/test")) return { ok: true, httpStatus: 200, latencyMs: 22 };
    if (url.endsWith("/busy-model/test")) return { ok: false, httpStatus: 429, latencyMs: 19, errorKeyword: "provider_rate_limited" };
    throw new Error("Unexpected test request");
  } });
  const subject = renderer(ModelSettings, { workspace: env.workspace, onModelsChanged: async () => {} }, env);
  await subject.settle();
  button(subject.tree, provider.displayName).props.onClick();
  await subject.settle();
  const testButtons = () => nodes(subject.tree, node => node.type === "button" && node.props.children === "interface.test");
  testButtons()[0].props.onClick();
  await subject.settle();
  const status = () => nodes(subject.tree, node => node.props.role === "status")[0].props.children;
  assert.match(status(), /HTTP 200/);
  testButtons()[1].props.onClick();
  await subject.settle();
  assert.match(status(), /HTTP 429/);
  subject.unmount();
});
test("an unowned model provider from stale metadata cannot be managed", async () => {
  const { env, requests } = modelEnvironment({ ...provider, id: "shared", scope: "platform", writable: false });
  const subject = renderer(ModelSettings, { workspace: env.workspace, onClose() {}, onModelsChanged: async () => {} }, env);
  await subject.settle();
  assert.equal(nodes(subject.tree, node => node.type === "input" && node.props.name === "providerApiKey").length, 0);
  button(subject.tree, provider.displayName).props.onClick(); await subject.settle();
  assert.equal(button(subject.tree, "workspaceResources.createCopy"), undefined);
  assert.equal(nodes(subject.tree, node => node.type === "input" && node.props.name === "providerApiKey").length, 0);
  assert.equal(requests.filter(([, options]) => options?.method).length, 0);
  subject.unmount();
});
function pluginEnvironment(role = "admin", credentialScope = "workspace") {
  const requests = [];
  const env = environment({ workspace: { id: "one", role }, request: async (url, options) => {
    requests.push([url, options]);
    if (url.endsWith("/mcp-bearer-credentials")) return { credentials: [{ id: "credential", pluginName: "example", credentialRef: "service-key", version: 2, scope: credentialScope, writable: credentialScope === "workspace" }] };
    if (url.endsWith("/rotate")) return { credential: { id: "credential", pluginName: "example", credentialRef: "service-key", version: 3, scope: "workspace", writable: true } };
    if (url.endsWith("/plugins")) return { plugins: [plugin] };
    return { plugin };
  } });
  return { env, requests };
}
async function expandedPlugin(env) {
  const subject = renderer(PluginSettings, { workspace: env.workspace, isSuperuser: false }, env);
  await subject.settle();
  nodes(subject.tree, node => node.type === "button" && node.props["aria-expanded"] === false)[0].props.onClick();
  await subject.settle(); return subject;
}
test("workspace admin can rotate MCP credentials without retrieving an existing secret", async () => {
  const { env, requests } = pluginEnvironment();
  const subject = await expandedPlugin(env);
  assert.ok(requests.some(([url]) => url === "/api/workspaces/one/mcp-bearer-credentials"));
  let key = nodes(subject.tree, node => node.type === "input" && node.props.type === "password")[0];
  assert.ok(key); assert.equal(key.props.value, "");
  key.props.onChange({ target: { value: "replacement" } }); await subject.settle();
  nodes(subject.tree, node => node.type === "form")[0].props.onSubmit({ preventDefault() {} }); await subject.settle();
  assert.ok(requests.some(([url, options]) => url === "/api/workspaces/one/mcp-bearer-credentials/credential/rotate" && JSON.parse(options.body).secret === "replacement"));
  assert.equal(nodes(subject.tree, node => node.type === "input" && node.props.type === "password")[0].props.value, "");
  subject.unmount();
});
test("members cannot upload plugin packages or edit credentials", async () => {
  const { env, requests } = pluginEnvironment("member");
  const subject = await expandedPlugin(env);
  assert.equal(nodes(subject.tree, node => node.type === "input" && ["file", "password"].includes(node.props.type)).length, 0);
  assert.equal(requests.some(([url]) => url.includes("mcp-bearer-credentials")), false);
  subject.unmount();
});
test("the plugin detail switch changes workspace enablement for administrators", async () => {
  for (const role of ["admin", "member"]) {
    const { env, requests } = pluginEnvironment(role);
    const subject = renderer(PluginSettings, { workspace: env.workspace, catalog: { selectedName: plugin.name } }, env);
    await subject.settle();
    const toggle = () => nodes(subject.tree, node => node.props["aria-label"] === `pluginSettings.disable ${plugin.displayName}`)[0];
    if (role === "admin") {
      assert.equal(toggle().props.checked, true);
      toggle().props.onCheckedChange(false);
      await subject.settle();
      assert.ok(requests.some(([url, options]) => url === "/api/workspaces/one/plugins/example" && options?.method === "PATCH" && JSON.parse(options.body).enabled === false));
      assert.equal(toggle(), undefined);
    } else {
      assert.equal(toggle(), undefined);
    }
    subject.unmount();
  }
});
test("admin plugin upload uses workspace package endpoint and shared credential is overridden with a fresh key", async () => {
  const { env, requests } = pluginEnvironment("admin", "platform");
  const subject = await expandedPlugin(env);
  const upload = nodes(subject.tree, node => node.type === "input" && node.props.type === "file")[0];
  assert.ok(upload);
  await upload.props.onChange({ target: { files: [new Blob(["package"], { type: "application/zip" })] } }); await subject.settle();
  assert.ok(requests.some(([url, options]) => url === "/api/workspaces/one/plugins/upload" && options.body instanceof FormData));
  const key = nodes(subject.tree, node => node.type === "input" && node.props.type === "password")[0];
  key.props.onChange({ target: { value: "own-key" } }); await subject.settle();
  nodes(subject.tree, node => node.type === "form")[0].props.onSubmit({ preventDefault() {} }); await subject.settle();
  assert.ok(requests.some(([url, options]) => url === "/api/workspaces/one/mcp-bearer-credentials" && options?.method === "POST"));
  assert.equal(requests.some(([url]) => url.includes("/credential/rotate")), false);
  subject.unmount();
});

const harnessUrl = new URL("./componentHarness.mjs", import.meta.url).href;
const routeStubs = ["ModelSettings", "PluginSettings", "WorkspaceGroupsRoute", "WorkspaceMembersRoute", "EnvironmentSettings", "WorkspaceProfileSettings"].map(name => [`./${name}`, module(`export default function ${name}(){return null}`)]);
const routeLoad = subjectLoader({ extraOverrides: [
  ...routeStubs,
  ["./SessionArchive", module("export const SessionArchive=()=>null;")],
  ["./DeveloperSettings", module("export const DEVELOPER_DOC_GROUPS=[];export const developerTopic=()=>null;export const developerDocSearch=()=>'';export const DeveloperSettings=()=>null;")],
  ["react-router", module(`import {context} from ${JSON.stringify(harnessUrl)};export const Link=()=>null;export const Navigate=()=>null;export const useRouteLoaderData=id=>id==="authenticated"?{user:context.user}:context.workspace?{workspace:context.workspace}:null;export const useLoaderData=()=>({workspace:context.workspace});export const useLocation=()=>context.location;export const useSearchParams=()=>[new URLSearchParams(context.location.search)];export const useNavigate=()=>context.navigate;export const useOutletContext=()=>({});`)],
  ["../components/ExternalLogin", module("export const ExternalIdentitySettings=()=>null;")],
  ["../components/LanguageSelector", module("export const LanguageSelector=()=>null;")],
  ["../components/ThemeSelector", module("export const ThemeSelector=()=>null;")],
  ["../preferences", module("export const useEnterStartsNewLine=()=>false;export const writeEnterStartsNewLine=()=>{};")],
] });
const { default: SettingsPage } = await import(await routeLoad(fileURLToPath(new URL("../../src/routes/SettingsRoute.jsx", import.meta.url))));
test("workspace model settings allow owner and admin, reject member, and preserve platform role separation", () => {
  for (const role of ["owner", "admin", "member"]) {
    const env = environment({ workspace: { id: "one", role }, location: { pathname: "/w/one/settings/models", state: null } });
    const subject = renderer(SettingsPage, {}, env); subject.render();
    assert.equal(nodes(subject.tree, node => node.type.name === "ModelSettings").length, role === "member" ? 0 : 1);
    const platform = renderer(SettingsPage, {}, { ...env, location: { pathname: "/w/one/settings/platform-models", state: null } }); platform.render();
    assert.equal(nodes(platform.tree, node => node.type.name === "ModelSettings").length, 0);
  }
});
test("retired platform settings stay unavailable to superusers", () => {
  for (const section of ["platform-models", "global-plugins", "workspace-resources"]) {
    const env = environment({ workspace: null, user: { id: "operator", isSuperuser: true }, location: { pathname: "/settings/" + section, state: null } });
    const subject = renderer(SettingsPage, {}, env); subject.render();
    assert.equal(nodes(subject.tree, node => node.type.name === "ModelSettings").length, 0);
    assert.equal(nodes(subject.tree, node => node.props.to === "/settings/preferences").length, 1);
  }
});

test("uninstall removes only a workspace installation; shared packages retain only enablement control", async () => {
  const { env, requests } = pluginEnvironment();
  const subject = await expandedPlugin(env);
  button(subject.tree, "globalPluginSettings.remove").props.onClick(); await subject.settle();
  nodes(subject.tree, node => node.type.name === "ConfirmDialog" && node.props.open)[0].props.onConfirm(); await subject.settle();
  assert.ok(requests.some(([url, options]) => url === "/api/workspaces/one/plugins/example/installation" && options?.method === "DELETE"));
  subject.unmount();
  const sharedEnv = pluginEnvironment().env;
  const originalRequest = sharedEnv.request;
  sharedEnv.request = async (url, options) => {
    const result = await originalRequest(url, options);
    if (result.plugins) return { plugins: [{ ...plugin, scope: "platform", writable: false }] };
    if (result.plugin) return { plugin: { ...plugin, scope: "platform", writable: false } };
    return result;
  };
  const shared = await expandedPlugin(sharedEnv);
  assert.equal(button(shared.tree, "globalPluginSettings.remove"), undefined);
  assert.equal(nodes(shared.tree, node => node.type === "button" && node.props["aria-pressed"] === true)[0].props.disabled, false);
  shared.unmount();
});

test("package management explains that updates require a fresh environment without replacing existing memory", async () => {
  const { env } = pluginEnvironment();
  const subject = renderer(PluginSettings, { workspace: env.workspace, isSuperuser: false }, env);
  await subject.settle();
  assert.ok(nodes(subject.tree, node => typeof node.props.children === "string" && node.props.children === "Existing environments keep their mounted package version. Create a new environment or conversation after updating to use the new version.").length);
  subject.unmount();
});
